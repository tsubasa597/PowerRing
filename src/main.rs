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

// 没有控制台窗口：所有输出走 %APPDATA%\PowerRing\log.txt。
// 代价是调试时看不到实时输出，换来的是双击运行不再弹黑框。
#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod device;
mod hidpp;
mod log;
mod popup;
mod settings;
mod ui;
mod profile;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use hidapi::HidApi;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::config::Config;
use crate::popup::Line;

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

/// 弹窗里的行。色块颜色与托盘电量环共用同一套语义，
/// 用户看环和看面板得到的是同一个信号，不需要重新学一遍。
fn popup_lines(list: &[Status]) -> Vec<Line> {
    if list.is_empty() {
        return vec![Line {
            text: "未检测到设备".to_string(),
            swatch: (150, 150, 150),
        }];
    }
    list.iter()
        .map(|s| Line {
            text: match s.percent {
                Some(p) => format!(
                    "{}  {}%{}",
                    s.name,
                    p,
                    if s.charging { "  充电中" } else { "" }
                ),
                None => format!("{}  --", s.name),
            },
            swatch: swatch_of(s),
        })
        .collect()
}

/// 电量档位 → 颜色。与 ring_icon 内的配色保持一致，改动时两处一起改。
fn swatch_of(s: &Status) -> (u8, u8, u8) {
    match s.percent {
        None => (150, 150, 150),     // 未知：灰
        Some(_) if s.charging => (52, 152, 219), // 充电中：蓝
        Some(p) if p <= 10 => (192, 57, 43),     // 严重：红
        Some(p) if p <= 20 => (230, 126, 34),    // 警告：橙
        Some(_) => (39, 174, 96),                // 正常：绿
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 日志放在最前面：后面任何一步出问题，都要有地方可查
    log::init();

    // Windows：给进程一个明确的 AppUserModelID，托盘通知才会归属到 PowerRing
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        let _ = SetCurrentProcessExplicitAppUserModelID(windows::core::w!("PowerRing.PowerRing"));
    }

    // 声明 Per-Monitor DPI 感知：不做这一步，Windows 会把我们的窗口坐标按
    // 96 DPI 虚拟化，而托盘位置（Shell_NotifyIconGetRect）给的是物理像素 ——
    // 两套坐标一混，4K + 150% 缩放的机器上弹窗就飘到屏幕边上去了。
    // 副作用是系统不再帮我们拉伸界面，文字也会更清晰（代价是要自己按 DPI 放大字号）。
    // 必须放在创建任何窗口之前，晚了就不生效。
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        let ok = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        info!("[dpi] Per-Monitor V2 感知：{:?}", ok);
    }

    let cfg = Config::load();
    info!("[config] {:?}", cfg);

    let profiles = profile::load();
    info!("[profile] 已加载 {} 份设备声明", profiles.len());
    for p in &profiles {
        info!(
            "[profile] {} — {:04X}:{:04X}（{}，来源：{}）",
            p.name, p.vid, p.pid, p.verification, p.protocol_source
        );
    }

    let interval = cfg.poll_interval_secs.max(5);
    // 轮询线程每轮读 settings 里那个值，改完设置立刻生效，不用重启
    settings::set_interval(interval);

    // ---- 轮询线程：只管读设备，结果塞进 channel ----
    // hidapi 的设备句柄不能跨线程（Device 不是 Send），所以 HidApi 必须在线程内部创建，
    // 主线程从头到尾不碰 HID，只负责 UI 与消息泵。
    let (tx, rx) = mpsc::channel::<Vec<Status>>();
    // 「立即刷新」用的唤醒通道：轮询线程平时睡在 recv_timeout 上，收到信号就提前醒。
    // 没有它，「刷新」菜单项就得干等下一轮轮询（60 秒），点了跟没点一样。
    let (trig_tx, trig_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        struct Watched {
            dev: device::Device,
            percent: Option<u8>,
            charging: bool,
        }
        let api = match HidApi::new() {
            Ok(a) => a,
            Err(e) => {
                warn!("[hid] 初始化失败：{e}");
                return;
            }
        };
        let devices = device::scan(&api, &profiles);
        for d in &devices {
            info!("[scan] 发现设备：{}", d.name);
        }
        warn!("[scan] 首轮匹配到 {} 台设备", devices.len());

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
                        info!("[scan] 发现设备：{}", d.name);
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
                            info!("[battery] {} → {} mV {}%", w.dev.name, b.mv, b.percent);
                        }
                        w.percent = Some(smooth(w.percent, b.percent));
                        w.charging = b.charging;
                    }
                    // 读失败：保留上一次的值。绝不用 0% 表示「读不到」（PRD §2.3）
                    None => warn!("[battery] {} 读取失败，保留上次读数", w.dev.name),
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
            // 只要还有设备从没读出来过，就用 5 秒短间隔重试，而不是干等一整轮。
            // 实测：开机或上一进程被强杀后，接收器会有几十秒不响应，
            // 若按 60 秒轮询，面板会长时间挂着「--」，看起来像坏了。
            let still_pending =
                watched.is_empty() || watched.iter().any(|w| w.percent.is_none());
            let wait = if still_pending { 5 } else { settings::interval() };
            // 睡觉改成带唤醒的等待：有人点「立即刷新」就立刻进入下一轮
            match trig_rx.recv_timeout(Duration::from_secs(wait as u64)) {
                Ok(()) => {
                    // 连点几次会攒下多个信号，先排空，免得接下来几轮白转
                    while trig_rx.try_recv().is_ok() {}
                    info!("[poll] 收到刷新请求，立即读一次");
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    // ---- 托盘：必须在跑消息泵的那个线程上创建 ----
    // 菜单里只有「退出」是不够用的，实测用户左右键点下去都直奔退出：
    // tray-icon 默认左键也会弹菜单（menu_on_left_click 默认 true），
    // 于是左键的弹窗和系统菜单同时出现，而菜单里又只有退出一项 —— 左键等于被吃掉。
    // 修法：左键纯弹窗（关掉它的菜单），右键保留系统菜单并给足内容。
    let menu = Menu::new();
    let open_panel = MenuItem::new("显示电量面板", true, None);
    let refresh = MenuItem::new("立即刷新", true, None);
    let open_settings = MenuItem::new("设置…", true, None);
    let quit = MenuItem::new("退出 PowerRing", true, None);
    menu.append(&open_panel)?;
    menu.append(&refresh)?;
    menu.append(&open_settings)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;

    let quitting = Arc::new(AtomicBool::new(false));
    // 菜单线程拿不到 TrayIcon（它不是 Send），只能把意图传回主线程，由主线程真正去做
    let panel_requested = Arc::new(AtomicBool::new(false));
    let settings_requested = Arc::new(AtomicBool::new(false));
    let panel_id = open_panel.id().clone();
    let refresh_id = refresh.id().clone();
    let settings_id = open_settings.id().clone();
    let quit_id = quit.id().clone();
    let flag = quitting.clone();
    let req = panel_requested.clone();
    let sreq = settings_requested.clone();
    let trigger = trig_tx.clone();
    // MenuEvent 只有在消息泵跑起来之后才会产生，所以这里只是「接」，不是「驱动」
    thread::spawn(move || {
        for ev in MenuEvent::receiver().iter() {
            if ev.id() == &panel_id {
                req.store(true, Ordering::SeqCst);
            } else if ev.id() == &refresh_id {
                let _ = trigger.send(());
            } else if ev.id() == &settings_id {
                sreq.store(true, Ordering::SeqCst);
            } else if ev.id() == &quit_id {
                info!("[tray] 收到退出命令");
                flag.store(true, Ordering::SeqCst);
            }
        }
    });

    let tray = TrayIconBuilder::new()
        .with_tooltip("PowerRing 启动中")
        .with_icon(ring_icon(None, false))
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?;

    // 托盘图标的屏幕位置只在 TrayIconEvent 里带出来，菜单事件里没有，
    // 所以右键点过的那一瞬间把它存下来，给菜单里的「显示电量面板」当锚点。
    let tray_rect: Arc<Mutex<Option<(i32, i32, i32, i32)>>> = Arc::new(Mutex::new(None));

    // 弹窗窗口必须和消息泵同线程，否则 WM_PAINT 永远到不了
    popup::create()?;
    settings::create()?;

    info!("[tray] 托盘已就绪，每 {} 秒刷新一次（消息泵已启动）", interval);

    // ---- 主线程：消息泵 + 渲染 ----
    let mut latest: Vec<Status> = Vec::new();
    let rx: Receiver<Vec<Status>> = rx;
    while !quitting.load(Ordering::SeqCst) {
        unsafe { pump_messages() };

        // 点击托盘。与 MenuEvent 不同，这里用 try_recv 而非独立线程：
        // 主循环本来就 20Hz 在转，顺手收一下即可，省一个线程。
        // 按键分工：左键 = 开关电量面板；右键 = 系统菜单（由 tray-icon 自己弹），
        // 我们只负责在菜单要冒出来之前把面板收掉，免得置顶面板挡住菜单。
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button,
                button_state,
                rect,
                ..
            } = ev
            {
                if let Ok(mut g) = tray_rect.lock() {
                    *g = Some((
                        rect.position.x as i32,
                        rect.position.y as i32,
                        rect.size.width as i32,
                        rect.size.height as i32,
                    ));
                }
                info!(
                    "[click] {:?} {:?} rect=({},{}) size=({},{}) 面板当前可见={}",
                    button,
                    button_state,
                    rect.position.x,
                    rect.position.y,
                    rect.size.width,
                    rect.size.height,
                    popup::is_visible()
                );
                match (button, button_state) {
                    (MouseButton::Left, MouseButtonState::Up) => {
                        if popup::is_visible() {
                            popup::hide();
                        } else {
                            popup::show(
                                popup_lines(&latest),
                                rect.position.x as i32,
                                rect.position.y as i32,
                                rect.size.width as i32,
                                rect.size.height as i32,
                            );
                            info!("[popup] 显示 {} 台设备", latest.len());
                        }
                    }
                    (MouseButton::Right, MouseButtonState::Down) => popup::hide(),
                    _ => {}
                }
            }
        }

        // 菜单里的「显示电量面板」：真正的弹窗动作必须留在这个线程（窗口和泵同线程），
        // 菜单线程只负责把「有人点了」这个事实递过来。
        if panel_requested.swap(false, Ordering::SeqCst) {
            if popup::is_visible() {
                popup::hide();
            } else {
                let anchor = tray_rect.lock().ok().and_then(|g| *g);
                match anchor {
                    Some((x, y, w, h)) => popup::show(popup_lines(&latest), x, y, w, h),
                    // 一次托盘点击都没发生过（比如刚启动就没人点过图标），退回光标位置
                    None => popup::show_near_cursor(popup_lines(&latest)),
                }
                info!("[popup] 菜单打开面板，共 {} 台设备", latest.len());
            }
        }

        // 「设置…」同理：窗口必须和消息泵同线程，打开动作也留在这里
        if settings_requested.swap(false, Ordering::SeqCst) {
            settings::open();
        }

        popup::tick();

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
                warn!("[tray] set_icon 失败：{e}");
            }
            if let Err(e) = tray.set_tooltip(Some(&tip)) {
                warn!("[tray] set_tooltip 失败：{e}");
            }
            info!("[tray] tooltip = {:?}", tip);

            // 面板开着时，新读数直接刷到面板上（用户点开它就该看到最新值）
            if popup::is_visible() {
                popup::refresh(popup_lines(&latest));
            }
        }

        // 20Hz 轮询消息队列：既保证右键即时响应，空闲 CPU 也几乎为零
        thread::sleep(Duration::from_millis(50));
    }

    // 正常退出：让 TrayIcon 走到 drop，托盘图标才不会变成幽灵残影
    info!("[tray] 正在退出");
    drop(tray);
    Ok(())
}
