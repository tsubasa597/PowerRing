//! PowerRing M1（第一块）：从声明驱动的多设备轮询
//!
//! 与 M0.5 的区别：设备不再硬编码，全部来自 devices/*.yaml 声明；
//! 支持多设备；读数做滞回平滑；读不到时显示「未知」而不是 0%。
//!
//! 线程模型（2026-10-20 修右键菜单时确立）：
//!   · 主线程  —— 创建托盘并跑 Win32 消息泵。tray-icon 自己不带事件循环，
//!                右键菜单靠 Shell 投递的 WM_RBUTTONUP 触发，没泵就永远不弹。
//!   · 轮询线程 —— 读写 HID、算百分比，结果通过 channel 交给主线程渲染。
//!   · 菜单线程 —— 监听 MenuEvent，只负责把退出标志置起来。
//! 把 HID 读写挪出主线程，是为了让消息泵不被几百毫秒的设备响应卡住。

mod config;
mod device;
mod hidpp;
mod profile;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use hidapi::HidApi;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::config::Config;

/// 轮询线程交给主线程的一次快照
struct Status {
    name: String,
    percent: Option<u8>,
    charging: bool,
}

/// 手绘电量环图标：32×32 RGBA，零图像处理依赖（这正是「PowerRing」的由来）
/// percent 为 None 表示「未知」—— 画成全灰环，绝不用 0% 冒充读不到（PRD F2）
fn ring_icon(percent: Option<u8>, charging: bool) -> Icon {
    const S: usize = 32;
    let mut rgba = vec![0u8; S * S * 4];
    let c = (S as f32 - 1.0) / 2.0;
    let r_out = 15.2_f32;
    let r_in = 10.8_f32;
    let filled_deg = percent.unwrap_or(0).min(100) as f32 / 100.0 * 360.0;

    for y in 0..S {
        for x in 0..S {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let d = (dx * dx + dy * dy).sqrt();
            if d < r_in || d > r_out {
                continue;
            }
            // 以 12 点方向为 0°，顺时针填充
            let mut ang = dy.atan2(dx).to_degrees() + 90.0;
            if ang < 0.0 {
                ang += 360.0;
            }
            let (r, g, b) = match percent {
                None => (110, 110, 110), // 未知：整环灰
                Some(p) if ang <= filled_deg => {
                    if charging {
                        (52, 152, 219) // 充电中：蓝
                    } else if p <= 10 {
                        (192, 57, 43) // 严重：红
                    } else if p <= 20 {
                        (230, 126, 34) // 警告：橙
                    } else {
                        (39, 174, 96) // 正常：绿
                    }
                }
                Some(_) => (75, 75, 75), // 未填充部分：深灰
            };
            let i = (y * S + x) * 4;
            rgba[i..i + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
    Icon::from_rgba(rgba, S as u32, S as u32).expect("构造图标失败")
}

/// 读数滞回：变化不足 2 个百分点就不动。
/// 原因（M0.5 实测）：电压有 1mV 抖动，正好压在放电曲线档位边界上，
/// 不做平滑会让百分比在 27% 和 28% 之间反复横跳。
fn smooth(shown: Option<u8>, fresh: u8) -> u8 {
    match shown {
        None => fresh,
        Some(s) if (fresh as i16 - s as i16).abs() >= 2 => fresh,
        Some(s) => s,
    }
}

/// 取走并处理本线程消息队列里的所有消息。
///
/// 这是右键菜单能弹出的**唯一**前提：tray-icon（0.26）在 Windows 上不自带事件循环
/// （其源码内没有任何 GetMessageW）。set_icon / set_tooltip 之所以一直正常，是因为它们
/// 直接调 Shell_NotifyIconW，不走消息队列；而右键点击是 Shell 把 WM_RBUTTONUP 投递到
/// tray-icon 的隐藏窗口，只有把消息取出来派发，它内部的 wndproc 才会调 show_tray_menu。
unsafe fn pump_messages() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };

    let mut msg: MSG = std::mem::zeroed();
    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

fn tooltip_of(list: &[Status]) -> String {
    if list.is_empty() {
        return "PowerRing — 未检测到设备".to_string();
    }
    list.iter()
        .map(|s| match s.percent {
            Some(p) => format!("{} {}%{}", s.name, p, if s.charging { " 充电中" } else { "" }),
            None => format!("{} --", s.name),
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Windows：给进程一个明确的 AppUserModelID，托盘通知才会归属到 PowerRing
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        let _ = SetCurrentProcessExplicitAppUserModelID(windows::core::w!("PowerRing.PowerRing"));
    }

    let cfg = Config::load();
    println!("[config] {:?}", cfg);

    let profiles = profile::load("devices");
    println!("[profile] 已加载 {} 份设备声明", profiles.len());
    for p in &profiles {
        println!(
            "[profile] {} — {:04X}:{:04X}（{}，来源：{}）",
            p.name, p.vid, p.pid, p.verification, p.protocol_source
        );
    }

    let interval = cfg.poll_interval_secs.max(5);

    // ---- 轮询线程：只管读设备，结果塞进 channel ----
    // hidapi 的设备句柄不能跨线程（Device 不是 Send），所以 HidApi 必须在线程内部创建，
    // 主线程从头到尾不碰 HID，只负责 UI 与消息泵。
    let (tx, rx) = mpsc::channel::<Vec<Status>>();
    thread::spawn(move || {
        struct Watched {
            dev: device::Device,
            percent: Option<u8>,
            charging: bool,
        }
        let api = match HidApi::new() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[hid] 初始化失败：{e}");
                return;
            }
        };
        let devices = device::scan(&api, &profiles);
        for d in &devices {
            println!("[scan] 发现设备：{}", d.name);
        }
        eprintln!("[scan] 首轮匹配到 {} 台设备", devices.len());

        let mut watched: Vec<Watched> = devices
            .into_iter()
            .map(|d| Watched {
                dev: d,
                percent: None,
                charging: false,
            })
            .collect();

        loop {
            // 首轮没扫到不代表没有设备：实测上一进程被强杀后，接收器会有几秒不响应。
            // 所以只要列表是空的，每轮都重试一次，而不是一次扫不到就判死刑。
            if watched.is_empty() {
                let fresh = device::scan(&api, &profiles);
                if !fresh.is_empty() {
                    for d in &fresh {
                        println!("[scan] 发现设备：{}", d.name);
                    }
                    watched = fresh
                        .into_iter()
                        .map(|d| Watched {
                            dev: d,
                            percent: None,
                            charging: false,
                        })
                        .collect();
                }
            }

            for w in watched.iter_mut() {
                match w.dev.read() {
                    Some(b) => {
                        if w.percent.map(|p| p != b.percent).unwrap_or(true) {
                            println!("[battery] {} → {} mV {}%", w.dev.name, b.mv, b.percent);
                        }
                        w.percent = Some(smooth(w.percent, b.percent));
                        w.charging = b.charging;
                    }
                    // 读失败：保留上一次的值。绝不用 0% 表示「读不到」（PRD §2.3）
                    None => eprintln!("[battery] {} 读取失败，保留上次读数", w.dev.name),
                }
            }
            let snap = watched
                .iter()
                .map(|w| Status {
                    name: w.dev.name.clone(),
                    percent: w.percent,
                    charging: w.charging,
                })
                .collect::<Vec<_>>();
            if tx.send(snap).is_err() {
                break; // 主线程已退出
            }
            thread::sleep(Duration::from_secs(interval as u64));
        }
    });

    // ---- 托盘：必须在跑消息泵的那个线程上创建 ----
    let menu = Menu::new();
    let quit = MenuItem::new("退出 PowerRing", true, None);
    menu.append(&quit)?;

    let quitting = Arc::new(AtomicBool::new(false));
    let quit_id = quit.id().clone();
    let flag = quitting.clone();
    // MenuEvent 只有在消息泵跑起来之后才会产生，所以这里只是「接」，不是「驱动」
    thread::spawn(move || {
        for ev in MenuEvent::receiver().iter() {
            if ev.id() == &quit_id {
                println!("[tray] 收到退出命令");
                flag.store(true, Ordering::SeqCst);
            }
        }
    });

    let tray = TrayIconBuilder::new()
        .with_tooltip("PowerRing 启动中")
        .with_icon(ring_icon(None, false))
        .with_menu(Box::new(menu))
        .build()?;

    println!("[tray] 托盘已就绪，每 {} 秒刷新一次（消息泵已启动）", interval);

    // ---- 主线程：消息泵 + 渲染 ----
    let mut latest: Vec<Status> = Vec::new();
    let rx: Receiver<Vec<Status>> = rx;
    while !quitting.load(Ordering::SeqCst) {
        unsafe { pump_messages() };

        let mut changed = false;
        while let Ok(snap) = rx.try_recv() {
            latest = snap;
            changed = true;
        }

        if changed {
            // tooltip 逐台列出；托盘图标取**电量最低**的那台（最需要被看到的那个）
            let tip = tooltip_of(&latest);
            let lowest = latest.iter().filter_map(|s| s.percent).min();
            let any_charging = latest.iter().any(|s| s.charging);

            if let Err(e) = tray.set_icon(Some(ring_icon(lowest, any_charging))) {
                eprintln!("[tray] set_icon 失败：{e}");
            }
            if let Err(e) = tray.set_tooltip(Some(&tip)) {
                eprintln!("[tray] set_tooltip 失败：{e}");
            }
            println!("[tray] tooltip = {:?}", tip);
        }

        // 20Hz 轮询消息队列：既保证右键即时响应，空闲 CPU 也几乎为零
        thread::sleep(Duration::from_millis(50));
    }

    // 正常退出：让 TrayIcon 走到 drop，托盘图标才不会变成幽灵残影
    println!("[tray] 正在退出");
    drop(tray);
    Ok(())
}
