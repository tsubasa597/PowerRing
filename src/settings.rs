//! F11 设置窗口：刷新间隔 + 两档低电量阈值 + 提醒开关 + 开机自启，保存后立即生效
//!
//! 控件用的是 Win32 标准子控件（STATIC / EDIT / BUTTON），不是自绘。
//! 理由和托盘面板一样（§8.4 ADR-001）：不引 GUI 框架，编译速度优先。
//! 标准控件的代价是外观朴素（系统经典样式），换来的是零依赖、零自绘代码。
//!
//! M1 只做「改了就能立刻用」的三项；M2 补上开机自启（要动注册表）。
//! 自启这一项的显示状态每次都重新问注册表（autostart::is_enabled），
//! 不认 config —— 用户可能在任务管理器里禁掉过它，那份开关 config 看不到。

use crate::config::Config;
use crate::ui::{create_font, font_px as ui_font_px, px};
use crate::info;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU8, Ordering};

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowRect,
    GetWindowTextW, RegisterClassW, SendMessageW, SetWindowPos, SetWindowTextW, ShowWindow,
    WNDCLASSW, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, HMENU, HWND_TOP, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WS_BORDER, WS_CAPTION, WS_CHILD, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE, WM_CLOSE, WM_COMMAND, WM_SETFONT,
};

// 复选框的勾选状态。常量本体在 Win32_UI_Controls 里，为两个整数再开一个 feature 不值
const BM_GETCHECK: u32 = 240;
const BM_SETCHECK: u32 = 241;
const BST_CHECKED: usize = 1;
const BST_UNCHECKED: usize = 0;

// 控件 ID：子窗口 ID 是塞在 CreateWindowExW 的 hMenu 位里的，必须自己编号
const ID_INTERVAL: isize = 1001;
const ID_WARN: isize = 1002;
const ID_CRITICAL: isize = 1003;
const ID_SAVE: isize = 1010;
const ID_CANCEL: isize = 1011;
const ID_STATUS: isize = 1020;
const ID_NOTIFY: isize = 1030;
const ID_TEST: isize = 1040;
const ID_AUTO: isize = 1050;

// 96 DPI 下的**客户区**布局，实际按 DPI 放大。
// 注意这是客户区（放控件的那块），不是窗口外框 —— 外框还要再加标题栏和边框，
// 由 outer_size() 负责换算，别直接拿它去 CreateWindowExW。
const BASE_W: f32 = 340.0;
const BASE_H: f32 = 286.0;
const MARGIN: f32 = 16.0;
const ROW_TOP: f32 = 20.0;
const ROW_PITCH: f32 = 36.0;
const LABEL_W: f32 = 178.0;
const EDIT_X: f32 = 200.0;
const EDIT_W: f32 = 124.0;
const EDIT_H: f32 = 24.0;
/// 复选框紧跟三行输入框
const CHECK_Y: f32 = ROW_TOP + ROW_PITCH * 3.0;
const CHECK_H: f32 = 24.0;
/// 复选框给它右边的「试一次」按钮让出位置
const CHECK_W: f32 = BASE_W - MARGIN * 2.0 - 68.0;
const TEST_W: f32 = 60.0;
const TEST_X: f32 = MARGIN + CHECK_W + 8.0;
/// 开机自启紧跟提醒那一行
const CHECK2_Y: f32 = CHECK_Y + 30.0;
const STATUS_Y: f32 = 192.0;
const BTN_Y: f32 = 236.0;
const BTN_W: f32 = 84.0;
const BTN_H: f32 = 30.0;
const BTN_GAP: f32 = 16.0;

static HW: AtomicIsize = AtomicIsize::new(0);
static HW_INTERVAL: AtomicIsize = AtomicIsize::new(0);
static HW_WARN: AtomicIsize = AtomicIsize::new(0);
static HW_CRITICAL: AtomicIsize = AtomicIsize::new(0);
static HW_STATUS: AtomicIsize = AtomicIsize::new(0);
static HW_NOTIFY: AtomicIsize = AtomicIsize::new(0);
static HW_AUTO: AtomicIsize = AtomicIsize::new(0);
/// 当前生效的刷新间隔。轮询线程每轮直接读它，改完设置不用重启程序
static INTERVAL: AtomicU32 = AtomicU32::new(60);
/// 当前生效的两档低电量阈值。电量环配色和提醒判断都读它，
/// 所以改完设置颜色立刻跟着变，不用重启
static LOW_WARN: AtomicU8 = AtomicU8::new(20);
static LOW_CRITICAL: AtomicU8 = AtomicU8::new(10);
/// 低电量提醒开关。关掉只是不弹卡片，阈值仍驱动电量环配色
static NOTIFY: AtomicBool = AtomicBool::new(true);
/// 设置窗口里点了「试一次」
static TEST_REQUESTED: AtomicBool = AtomicBool::new(false);

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

pub fn low_warn() -> u8 {
    LOW_WARN.load(Ordering::SeqCst)
}

pub fn low_critical() -> u8 {
    LOW_CRITICAL.load(Ordering::SeqCst)
}

pub fn set_low(warn: u8, critical: u8) {
    LOW_WARN.store(warn, Ordering::SeqCst);
    LOW_CRITICAL.store(critical, Ordering::SeqCst);
}

pub fn notify_enabled() -> bool {
    NOTIFY.load(Ordering::SeqCst)
}

pub fn set_notify(on: bool) {
    NOTIFY.store(on, Ordering::SeqCst);
}

/// 主循环每轮问一次：设置窗口里的「试一次」被点过吗。
/// 取走即清空，连点只会弹一张
pub fn take_test_request() -> bool {
    TEST_REQUESTED.swap(false, Ordering::SeqCst)
}


/// 普通窗口样式，**不带 WS_EX_TOOLWINDOW**。
///
/// 工具窗口样式是给浮动工具栏/调色板用的：它的标题栏是矮的，
/// 但系统给它预留的非客户区仍是标准标题栏的高度（144 DPI 下 45px），
/// 矮标题栏摆在 45px 里，右上角 X 的上方就空出一条。
/// 设置窗口是有标题的对话框，用标准样式才对 —— 代价是任务栏会多一个按钮，
/// 换来的是窗口被别的窗口盖住时还能在任务栏找回来。
fn window_style() -> WINDOW_STYLE {
    WS_POPUP | WS_CAPTION | WS_SYSMENU
}
/// 客户区尺寸 → 窗口外框尺寸。中间差的是标题栏和边框，
/// 这一步交给 AdjustWindowRectEx 算，别自己加常数：
/// 标题栏高度随 DPI 变，硬编码的常数换了台显示器就错。
fn outer_size() -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: px(BASE_W),
        bottom: px(BASE_H),
    };
    unsafe {
        if AdjustWindowRectEx(&mut r, window_style(), false, WINDOW_EX_STYLE(0)).is_err() {
            // 算不出来就退回加点余量，总比把按钮切掉强
            return (px(BASE_W) + 8, px(BASE_H) + 40);
        }
    }
    (r.right - r.left, r.bottom - r.top)
}


/// 把客户区撑到设计尺寸。
///
/// AdjustWindowRectEx 用的是**系统 DPI** 下的非客户区尺寸，而我们的窗口是
/// Per-Monitor 感知的：它一旦落在 4K + 150% 那块屏上，标题栏会比系统 DPI 算出来的
/// 高出一截，客户区就被啃掉 —— 表现就是底部的按钮被切掉一截。
/// 这里不猜，直接实测客户区，差多少补多少。
fn fit_client() {
    unsafe {
        let mut c = RECT::default();
        if GetClientRect(hwnd(), &mut c).is_err() {
            return;
        }
        let need_w = px(BASE_W);
        let need_h = px(BASE_H);
        let dw = (need_w - (c.right - c.left)).max(0);
        let dh = (need_h - (c.bottom - c.top)).max(0);
        info!(
            "[settings] 客户区实测 {}x{}，需要 {}x{}，补 {}x{}",
            c.right - c.left,
            c.bottom - c.top,
            need_w,
            need_h,
            dw,
            dh
        );
        if dw == 0 && dh == 0 {
            return;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd(), &mut r).is_err() {
            return;
        }
        let _ = SetWindowPos(
            hwnd(),
            Some(HWND_TOP),
            r.left,
            r.top,
            (r.right - r.left) + dw,
            (r.bottom - r.top) + dh,
            Default::default(),
        );
    }
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

        let (ow, oh) = outer_size();
        let wnd = CreateWindowExW(
            WINDOW_EX_STYLE(0), // 不用 WS_EX_TOOLWINDOW：矮标题栏会留空档，见 window_style 注释
            w!("PowerRingSettings"),
            w!("PowerRing 设置"),
            window_style(),
            0,
            0,
            ow,
            oh,
            None,
            None,
            None,
            None,
        )?;
        HW.store(wnd.0 as isize, Ordering::SeqCst);

        // 9 磅 = Windows 标准界面字号
        let font = create_font(ui_font_px(9.0));
        // 字号和缩放写进日志：换台显示器说「字太小」时，看这一行就能知道算成了多少
        info!(
            "[settings] 缩放 {:.2}x，9 磅 = {}px，客户区 {}x{}",
            crate::ui::scale(),
            ui_font_px(9.0),
            px(BASE_W),
            px(BASE_H)
        );
        let rows = [
            (ID_INTERVAL, "刷新间隔（秒）"),
            (ID_WARN, "低电量警告阈值（%）"),
            (ID_CRITICAL, "低电量严重阈值（%）"),
        ];
        for (i, (id, label)) in rows.iter().enumerate() {
            let y = px(ROW_TOP) + i as i32 * px(ROW_PITCH);
            let lb = make(
                w!("STATIC"),
                label,
                WS_CHILD | WS_VISIBLE,
                px(MARGIN),
                y + px(4.0),
                px(LABEL_W),
                px(20.0),
                wnd,
                0,
            );
            let ed = make(
                w!("EDIT"),
                "",
                WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP,
                px(EDIT_X),
                y,
                px(EDIT_W),
                px(EDIT_H),
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

        // BS_AUTOCHECKBOX：勾选状态由控件自己切换，我们只负责读
        let notify = make(
            w!("BUTTON"),
            "低电量时提醒我",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            px(MARGIN),
            px(CHECK_Y),
            px(CHECK_W),
            px(CHECK_H),
            wnd,
            ID_NOTIFY,
        );
        set_font(notify, font);
        HW_NOTIFY.store(notify.0 as isize, Ordering::SeqCst);

        // 提醒是「跌破阈值时才发生」的事件，鼠标不会说低就低，
        // 光靠设置开关用户根本没法验证它长什么样、弹在哪。
        // 给个手动入口，比让他等鼠标真的放电实在
        let test = make(
            w!("BUTTON"),
            "试一次",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            px(TEST_X),
            px(CHECK_Y),
            px(TEST_W),
            px(CHECK_H),
            wnd,
            ID_TEST,
        );
        set_font(test, font);

        // 开机自启整行占满，它右边没有配套按钮
        let auto = make(
            w!("BUTTON"),
            "开机时自动启动",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            px(MARGIN),
            px(CHECK2_Y),
            px(BASE_W - MARGIN * 2.0),
            px(CHECK_H),
            wnd,
            ID_AUTO,
        );
        set_font(auto, font);
        HW_AUTO.store(auto.0 as isize, Ordering::SeqCst);

        let status = make(
            w!("STATIC"),
            "",
            WS_CHILD | WS_VISIBLE,
            px(MARGIN),
            px(STATUS_Y),
            px(BASE_W - MARGIN * 2.0),
            px(20.0),
            wnd,
            ID_STATUS,
        );
        set_font(status, font);
        HW_STATUS.store(status.0 as isize, Ordering::SeqCst);

        // 两个按钮右对齐到边距，取消在右
        let bx = px(BASE_W - MARGIN - BTN_W - BTN_GAP - BTN_W);
        let save = make(
            w!("BUTTON"),
            "保存",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            bx,
            px(BTN_Y),
            px(BTN_W),
            px(BTN_H),
            wnd,
            ID_SAVE,
        );
        let cancel = make(
            w!("BUTTON"),
            "取消",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            bx + px(BTN_W + BTN_GAP),
            px(BTN_Y),
            px(BTN_W),
            px(BTN_H),
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
        set_check(ctl(&HW_NOTIFY), cfg.notify_enabled);

        // 自启这一格的勾选状态问注册表，不问 config —— 用户可能在设置之外改过它
        // （任务管理器的「启动」页、或者清理软件删过启动项），config 不知道那些事
        let auto = crate::autostart::is_enabled();
        set_check(ctl(&HW_AUTO), auto);
        if cfg.autostart != auto {
            info!(
                "[settings] 自启意图（config）={} 但注册表实际={}，按注册表显示",
                cfg.autostart, auto
            );
        }

        // 居中于虚拟桌面，双屏时才不会跑到某一块的角落
        let (vx, vy, vw, vh) = crate::ui::virtual_screen();
        let (w, h) = outer_size();
        let x = vx + (vw - w) / 2;
        let y = vy + (vh - h) / 2;
        let _ = SetWindowPos(hwnd(), Some(HWND_TOP), x, y, w, h, Default::default());
        let _ = ShowWindow(hwnd(), SW_SHOW);
        // 先显示再校准：客户区只有窗口真实存在后才量得准
        fit_client();
        info!("[settings] 打开设置窗口，位置=({},{}) 外框={}x{}", x, y, w, h);
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
    let notify = is_checked(ctl(&HW_NOTIFY));
    // 打开窗口时没记下这个状态，这里重新问一次注册表：
    // 用户在窗口开着的那几十秒里完全可能从任务管理器里改了启动项
    let auto = is_checked(ctl(&HW_AUTO));
    let auto_was = crate::autostart::is_enabled();

    cfg.poll_interval_secs = interval;
    cfg.low_warn = warn;
    cfg.low_critical = critical;
    cfg.notify_enabled = notify;
    cfg.autostart = auto;
    cfg.save();
    set_interval(interval);
    set_low(warn, critical);
    set_notify(notify);

    // 这句提示有实际用处：勾回开关的当下不会补弹一张旧提醒（那次跨档早就过去了），
    // 不说清楚的话，用户点了保存却发现啥也没弹，只会以为开关没生效
    let mut tip: String = if notify {
        "已保存；电量再次跌破阈值的那一刻才会提醒".into()
    } else {
        "已保存；提醒已关闭，环色仍按阈值变化".into()
    };

    // 只在勾选状态真的变了才动注册表。没变还写一遍的话，
    // 会把「用户自己在外面加的、指向别的路径的启动项」覆盖掉 —— 那是他的设置，
    // 不该被我们这边的保存按钮抹掉
    if auto != auto_was {
        match crate::autostart::set(auto) {
            Ok(()) => {
                tip = if auto {
                    "已保存；已加入开机启动，下次登录时自动运行".into()
                } else {
                    "已保存；已从开机启动里移除".into()
                };
            }
            // 写失败（公司电脑的常见情况：注册表被策略挡住）值得盖掉默认提示 ——
            // 用户点的是「勾上」，界面却什么都不说，他只会以为已经生效
            Err(e) => {
                tip = format!("开机自启{}失败：{}", if auto { "开启" } else { "关闭" }, e);
            }
        }
    }
    set_status(&tip);
    info!(
        "[settings] 保存配置：间隔={}s 警告={}% 严重={}% 提醒={} 自启={}（原 {}）",
        interval, warn, critical, notify, auto, auto_was
    );
}

fn set_status(text: &str) {
    let t = wide_nul(text);
    unsafe {
        let _ = SetWindowTextW(ctl(&HW_STATUS), windows::core::PCWSTR(t.as_ptr()));
    }
}

fn set_check(hw: HWND, on: bool) {
    unsafe {
        let _ = SendMessageW(
            hw,
            BM_SETCHECK,
            Some(WPARAM(if on { BST_CHECKED } else { BST_UNCHECKED })),
            Some(LPARAM(0)),
        );
    }
}

fn is_checked(hw: HWND) -> bool {
    let r = unsafe {
        SendMessageW(hw, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0)))
    };
    r.0 == BST_CHECKED as isize
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
                // 真正的弹卡片动作得留给主线程（窗口和消息泵同线程），
                // 这里只把「有人点了」这个事实递过去
                ID_TEST => TEST_REQUESTED.store(true, Ordering::SeqCst),
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
