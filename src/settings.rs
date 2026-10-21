//! F11 设置窗口：刷新间隔 + 两档低电量阈值，保存后立即生效
//!
//! 控件用的是 Win32 标准子控件（STATIC / EDIT / BUTTON），不是自绘。
//! 理由和托盘面板一样（§8.4 ADR-001）：不引 GUI 框架，编译速度优先。
//! 标准控件的代价是外观朴素（系统经典样式），换来的是零依赖、零自绘代码。
//!
//! M1 只做「改了就能立刻用」的三项。开机自启要动注册表，放到 M2 再做 ——
//! 现在的菜单里也就不给它留位置，免得摆一个点了没反应的开关。

use crate::config::Config;
use crate::ui::{create_font, px};
use crate::info;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowTextW, RegisterClassW, SendMessageW, SetWindowPos,
    SetWindowTextW, ShowWindow, WNDCLASSW, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, HMENU, HWND_TOP,
    SW_HIDE, SW_SHOW, WINDOW_STYLE, WS_BORDER, WS_CAPTION, WS_CHILD, WS_EX_TOOLWINDOW, WS_POPUP,
    WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WM_CLOSE, WM_COMMAND, WM_SETFONT,
};

// 控件 ID：子窗口 ID 是塞在 CreateWindowExW 的 hMenu 位里的，必须自己编号
const ID_INTERVAL: isize = 1001;
const ID_WARN: isize = 1002;
const ID_CRITICAL: isize = 1003;
const ID_SAVE: isize = 1010;
const ID_CANCEL: isize = 1011;
const ID_STATUS: isize = 1020;

// 96 DPI 下的布局，实际按 DPI 放大
const BASE_W: f32 = 340.0;
const BASE_H: f32 = 225.0;

static HW: AtomicIsize = AtomicIsize::new(0);
static HW_INTERVAL: AtomicIsize = AtomicIsize::new(0);
static HW_WARN: AtomicIsize = AtomicIsize::new(0);
static HW_CRITICAL: AtomicIsize = AtomicIsize::new(0);
static HW_STATUS: AtomicIsize = AtomicIsize::new(0);
/// 当前生效的刷新间隔。轮询线程每轮直接读它，改完设置不用重启程序
static INTERVAL: AtomicU32 = AtomicU32::new(60);

fn hwnd() -> HWND {
    HWND(HW.load(Ordering::SeqCst) as *mut core::ffi::c_void)
}
fn ctl(a: &AtomicIsize) -> HWND {
    HWND(a.load(Ordering::SeqCst) as *mut core::ffi::c_void)
}

pub fn interval() -> u32 {
    INTERVAL.load(Ordering::SeqCst)
}

pub fn set_interval(v: u32) {
    INTERVAL.store(v, Ordering::SeqCst);
}

/// 转成 UTF-16 并补 NUL：Win32 的字符串参数都以 NUL 结尾，不补会读到后面的内存
fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 注册窗口类、创建窗口和全部控件。必须和消息泵同线程。
pub fn create() -> Result<(), windows::core::Error> {
    // 窗口背景给白色：静态控件默认底色是按钮灰，不统一会很脏。
    // 刷子故意不 DeleteObject —— 它属于窗口类，进程活着就一直要用。
    let bg = unsafe { CreateSolidBrush(crate::ui::rgb(255, 255, 255)) };
    let class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        lpszClassName: w!("PowerRingSettings"),
        hbrBackground: bg,
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&class);

        let wnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("PowerRingSettings"),
            w!("PowerRing 设置"),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            0,
            0,
            px(BASE_W),
            px(BASE_H),
            None,
            None,
            None,
            None,
        )?;
        HW.store(wnd.0 as isize, Ordering::SeqCst);

        let font = create_font(px(9.0));
        let rows = [
            (ID_INTERVAL, "刷新间隔（秒）"),
            (ID_WARN, "低电量警告阈值（%）"),
            (ID_CRITICAL, "低电量严重阈值（%）"),
        ];
        for (i, (id, label)) in rows.iter().enumerate() {
            let y = px(20.0) + i as i32 * px(36.0);
            let lb = make(
                w!("STATIC"),
                label,
                WS_CHILD | WS_VISIBLE,
                px(16.0),
                y + px(4.0),
                px(178.0),
                px(20.0),
                wnd,
                0,
            );
            let ed = make(
                w!("EDIT"),
                "",
                WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP,
                px(200.0),
                y,
                px(124.0),
                px(24.0),
                wnd,
                *id,
            );
            set_font(lb, font);
            set_font(ed, font);
            match *id {
                ID_INTERVAL => HW_INTERVAL.store(ed.0 as isize, Ordering::SeqCst),
                ID_WARN => HW_WARN.store(ed.0 as isize, Ordering::SeqCst),
                _ => HW_CRITICAL.store(ed.0 as isize, Ordering::SeqCst),
            }
        }

        let status = make(
            w!("STATIC"),
            "",
            WS_CHILD | WS_VISIBLE,
            px(16.0),
            px(136.0),
            px(308.0),
            px(20.0),
            wnd,
            ID_STATUS,
        );
        set_font(status, font);
        HW_STATUS.store(status.0 as isize, Ordering::SeqCst);

        let save = make(
            w!("BUTTON"),
            "保存",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            px(136.0),
            px(166.0),
            px(84.0),
            px(30.0),
            wnd,
            ID_SAVE,
        );
        let cancel = make(
            w!("BUTTON"),
            "取消",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            px(232.0),
            px(166.0),
            px(84.0),
            px(30.0),
            wnd,
            ID_CANCEL,
        );
        set_font(save, font);
        set_font(cancel, font);
    }
    Ok(())
}

/// 打开窗口并填入当前配置
pub fn open() {
    let cfg = Config::load();
    unsafe {
        set_text(ctl(&HW_INTERVAL), &cfg.poll_interval_secs.to_string());
        set_text(ctl(&HW_WARN), &cfg.low_warn.to_string());
        set_text(ctl(&HW_CRITICAL), &cfg.low_critical.to_string());
        set_text(ctl(&HW_STATUS), "");

        // 居中于虚拟桌面，双屏时才不会跑到某一块的角落
        let (vx, vy, vw, vh) = crate::ui::virtual_screen();
        let w = px(BASE_W);
        let h = px(BASE_H);
        let x = vx + (vw - w) / 2;
        let y = vy + (vh - h) / 2;
        let _ = SetWindowPos(hwnd(), Some(HWND_TOP), x, y, w, h, Default::default());
        let _ = ShowWindow(hwnd(), SW_SHOW);
        info!("[settings] 打开设置窗口，位置=({},{})", x, y);
    }
}

fn hide() {
    unsafe {
        let _ = ShowWindow(hwnd(), SW_HIDE);
    }
}

/// 保存：三样都校验，不合法就地提示，绝不让脏值写进配置
fn on_save() {
    let interval = text_of(ctl(&HW_INTERVAL)).trim().parse::<u32>().ok();
    let warn = text_of(ctl(&HW_WARN)).trim().parse::<u8>().ok();
    let critical = text_of(ctl(&HW_CRITICAL)).trim().parse::<u8>().ok();

    let (interval, warn, critical) = match (interval, warn, critical) {
        (Some(i), Some(w), Some(c)) => (i, w, c),
        _ => {
            set_status("三个值都必须是整数");
            return;
        }
    };

    if !(5..=3600).contains(&interval) {
        set_status("刷新间隔要在 5 ~ 3600 秒之间");
        return;
    }
    if warn == 0 || warn > 100 || critical == 0 || critical >= warn {
        set_status("严重阈值要低于警告阈值，且都在 1 ~ 100 之间");
        return;
    }

    let mut cfg = Config::load();
    cfg.poll_interval_secs = interval;
    cfg.low_warn = warn;
    cfg.low_critical = critical;
    cfg.save();
    set_interval(interval);
    set_status("已保存，立即生效");
    info!(
        "[settings] 保存配置：间隔={}s 警告={}% 严重={}%",
        interval, warn, critical
    );
}

fn set_status(text: &str) {
    let t = wide_nul(text);
    unsafe {
        let _ = SetWindowTextW(ctl(&HW_STATUS), windows::core::PCWSTR(t.as_ptr()));
    }
}

fn text_of(hw: HWND) -> String {
    let mut buf = [0u16; 32];
    let n = unsafe { GetWindowTextW(hw, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn set_text(hw: HWND, text: &str) {
    let t = wide_nul(text);
    unsafe {
        let _ = SetWindowTextW(hw, windows::core::PCWSTR(t.as_ptr()));
    }
}

fn set_font(hw: HWND, font: windows::Win32::Graphics::Gdi::HFONT) {
    unsafe {
        let _ = SendMessageW(
            hw,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
}

/// 建一个子控件。id 为 0 表示不需要接收消息（纯标签）
fn make(
    class: windows::core::PCWSTR,
    text: &str,
    style: windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    parent: HWND,
    id: isize,
) -> HWND {
    // 窗口名要 NUL 结尾，w! 出来的字面量自带，这里自己拼的文本得补上
    let name = wide_nul(text);
    unsafe {
        CreateWindowExW(
            Default::default(),
            class,
            windows::core::PCWSTR(name.as_ptr()),
            style,
            x,
            y,
            w,
            h,
            Some(parent),
            Some(HMENU(id as *mut core::ffi::c_void)),
            None,
            None,
        )
        .unwrap_or(HWND(0 as *mut core::ffi::c_void))
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            // 子控件通知：wparam 低 16 位是控件 ID，高 16 位是通知码
            let id = (wparam.0 & 0xFFFF) as isize;
            match id {
                ID_SAVE => on_save(),
                ID_CANCEL => hide(),
                _ => {}
            }
            LRESULT(0)
        }
        // 标题栏的 ×：隐藏而不是销毁，控件留着下次接着用
        WM_CLOSE => {
            hide();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
