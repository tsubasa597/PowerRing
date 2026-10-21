//! F5 托盘弹窗：点击托盘图标，在图标上方弹出设备列表（名称 + 电量 + 充电状态）。
//!
//! 刻意不引入 GUI 框架 —— 整个弹窗是一个 WS_POPUP 窗口，内容在 WM_PAINT 里用
//! 几行 TextOutW 画出来。理由同 §8.4 ADR-001：本项目最大的成本是反馈循环速度
//! （AI 看不到屏幕，UI 只能靠用户截图往返），为一个几行文本的面板拖进 winit/wry，
//! 会让每次增量编译从 5 秒变成几十秒。等真需要复杂 UI 时再评估。
//!
//! 设计取舍：
//!   · 不抢焦点（SW_SHOWNA）—— 用户可能在打游戏或打字，抢焦点会打断。
//!     代价是无法用「失焦自动关闭」，所以关闭靠：再点一次托盘、点面板内部、10 秒超时。
//!   · WS_EX_TOOLWINDOW —— 不在任务栏留按钮，也不出现在 Alt+Tab 里。

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect,
    SelectObject, SetBkMode, SetTextColor, TextOutW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, FF_DONTCARE, HFONT, OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetCursorPos, GetSystemMetrics, RegisterClassW,
    SetWindowPos, ShowWindow, WNDCLASSW, WS_BORDER, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WM_LBUTTONDOWN, WM_PAINT, HWND_TOPMOST, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNA,
    SYSTEM_METRICS_INDEX,
};

/// 面板里的一行
pub struct Line {
    pub text: String,
    /// 左侧小色块的颜色，用来一眼区分电量档位
    pub swatch: (u8, u8, u8),
}

/// 面板几何：96 DPI 下的基准尺寸，实际绘制时按系统 DPI 放大。
/// 进程声明了 DPI 感知，系统不再替我们拉伸，不自己放大就只有一小条。
const BASE_WIDTH: f32 = 300.0;
const BASE_ROW_H: f32 = 28.0;
const BASE_PAD: f32 = 12.0;
const BASE_TOP: f32 = 30.0;
/// 没人管它就自己消失，避免一直挂在屏幕上
const AUTO_HIDE: Duration = Duration::from_secs(10);

// 下面四个 SM_* 是虚拟桌面（所有显示器拼起来的大矩形）的位置与尺寸。
// 直接写数值而不是引常量：windows crate 里它们散在别处，为四个数再多开依赖不值。
// 用虚拟桌面而不是主屏，是因为用户是双屏，托盘有可能在副屏上，
// 按主屏夹取会把它拽回主屏边缘。
const SM_XVIRTUALSCREEN: i32 = 76;
const SM_YVIRTUALSCREEN: i32 = 77;
const SM_CXVIRTUALSCREEN: i32 = 78;
const SM_CYVIRTUALSCREEN: i32 = 79;

/// 96 DPI = 1.0；4K + 150% 缩放 = 1.5
fn scale() -> f32 {
    let dpi = unsafe { GetDpiForSystem() };
    if dpi <= 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

fn width() -> i32 {
    (BASE_WIDTH * scale()).round() as i32
}
fn row_h() -> i32 {
    (BASE_ROW_H * scale()).round() as i32
}
fn pad() -> i32 {
    (BASE_PAD * scale()).round() as i32
}
fn top() -> i32 {
    (BASE_TOP * scale()).round() as i32
}

// 全局状态：wndproc 必须是 extern "system" 的自由函数，拿不到闭包上下文，
// 所以状态只能放静态。HWND 是裸指针包装、不保证 Sync，用整型存地址。
static HW: AtomicIsize = AtomicIsize::new(0);
static LINES: Mutex<Vec<(String, (u8, u8, u8))>> = Mutex::new(Vec::new());
static VISIBLE: AtomicBool = AtomicBool::new(false);
static SHOWN_AT: Mutex<Option<Instant>> = Mutex::new(None);

fn hwnd() -> HWND {
    HWND(HW.load(Ordering::SeqCst) as *mut core::ffi::c_void)
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// 造一个指定像素高度的字体。系统字库在 DPI 感知下不会自动变大，必须自己按 DPI 算；
/// 高度传负值 = 按字符本身的高度算（不含内部行距），排版更好控。
fn create_font(px: i32) -> HFONT {
    let face = w!("Microsoft YaHei UI");
    unsafe {
        CreateFontW(
            -px,
            0, // 宽度交给系统按字形自适应
            0,
            0,
            400, // FW_NORMAL
            0,   // 非斜体
            0,   // 无下划线
            0,   // 无删除线
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            FF_DONTCARE.0 as u32,
            face,
        )
    }
}

/// 虚拟桌面（所有显示器拼成的大矩形）的范围，用它来把面板夹在可见区域内
fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SYSTEM_METRICS_INDEX(SM_XVIRTUALSCREEN)),
            GetSystemMetrics(SYSTEM_METRICS_INDEX(SM_YVIRTUALSCREEN)),
            GetSystemMetrics(SYSTEM_METRICS_INDEX(SM_CXVIRTUALSCREEN)),
            GetSystemMetrics(SYSTEM_METRICS_INDEX(SM_CYVIRTUALSCREEN)),
        )
    }
}

/// 注册窗口类并创建一个初始隐藏的窗口。
/// 必须在跑消息泵的那个线程调用（窗口和泵必须同线程，否则消息永远到不了）。
pub fn create() -> Result<(), windows::core::Error> {
    let class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        lpszClassName: w!("PowerRingPopup"),
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&class);
        let hw = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("PowerRingPopup"),
            w!("PowerRing"),
            WS_POPUP | WS_BORDER,
            0,
            0,
            width(),
            top() + pad() * 2,
            None,
            None,
            None,
            None,
        )?;
        HW.store(hw.0 as isize, Ordering::SeqCst);
    }
    Ok(())
}

/// 在托盘图标正上方显示面板；放不下就翻到下方。
pub fn show(lines: Vec<Line>, icon_x: i32, icon_y: i32, icon_w: i32, icon_h: i32) {
    let w = width();
    let height = top() + pad() + lines.len() as i32 * row_h();
    if let Ok(mut g) = LINES.lock() {
        *g = lines
            .into_iter()
            .map(|l| (l.text, l.swatch))
            .collect::<Vec<_>>();
    }
    let hw = hwnd();
    let (vx, vy, vw, vh) = virtual_screen();
    let min_x = vx + 8;
    let min_y = vy + 8;
    // 托盘通常在屏幕右下角，默认往上弹；上方真的放不下才翻到下方
    let mut x = icon_x + icon_w / 2 - w / 2;
    let mut y = icon_y - height - 6;
    if y < min_y {
        y = icon_y + icon_h + 6;
    }
    // 夹在可见区域内：托盘贴着边时宁可整个面板往里挪，也不要被裁掉一块。
    // 夹取范围用虚拟桌面而不是主屏 —— 用户是双屏，托盘可能在副屏上。
    let max_x = (vx + vw - w - 8).max(min_x);
    let max_y = (vy + vh - height - 8).max(min_y);
    let target = (x, y);
    x = x.clamp(min_x, max_x);
    y = y.clamp(min_y, max_y);
    println!(
        "[popup] 托盘=({},{}) 尺寸 {}x{} → 想放=({},{}) 实际=({},{}) 桌面=({},{},{},{}）",
        icon_x,
        icon_y,
        w,
        height,
        target.0,
        target.1,
        x,
        y,
        vx,
        vy,
        vw,
        vh
    );
    unsafe {
        let _ = SetWindowPos(hw, Some(HWND_TOPMOST), x, y, w, height, SWP_NOACTIVATE);
        let _ = ShowWindow(hw, SW_SHOWNA);
        // 尺寸变了，强制重绘一次，否则会残留上一帧
        let _ = InvalidateRect(Some(hw), None, true);
    }
    VISIBLE.store(true, Ordering::SeqCst);
    if let Ok(mut g) = SHOWN_AT.lock() {
        *g = Some(Instant::now());
    }
}

/// 不知道托盘图标在哪时用这个：直接以当前光标位置为锚点。
/// 走右键菜单打开面板时就用它 —— 菜单项被点下的那一瞬间，光标就在菜单上、也就在图标旁边。
pub fn show_near_cursor(lines: Vec<Line>) {
    let mut pt = POINT { x: 0, y: 0 };
    let (cx, cy) = match unsafe { GetCursorPos(&mut pt) } {
        Ok(()) => (pt.x, pt.y),
        Err(_) => (0, 0),
    };
    show(lines, cx, cy, 0, 0);
}

pub fn hide() {
    if !VISIBLE.load(Ordering::SeqCst) {
        return;
    }
    unsafe { let _ = ShowWindow(hwnd(), SW_HIDE); };
    VISIBLE.store(false, Ordering::SeqCst);
    if let Ok(mut g) = SHOWN_AT.lock() {
        *g = None;
    }
}

pub fn is_visible() -> bool {
    VISIBLE.load(Ordering::SeqCst)
}

/// 弹窗开着时来了新读数：换掉内容并重绘。
/// 不重置超时计时 —— 面板只是被动刷新，不该因为它一直有人看着就永不消失。
pub fn refresh(lines: Vec<Line>) {
    if !VISIBLE.load(Ordering::SeqCst) {
        return;
    }
    if let Ok(mut g) = LINES.lock() {
        *g = lines
            .into_iter()
            .map(|l| (l.text, l.swatch))
            .collect::<Vec<_>>();
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd()), None, true);
    }
}

/// 主循环每轮调用：到点自动收起，省得用户忘了关
pub fn tick() {
    let expired = match SHOWN_AT.lock() {
        Ok(g) => g.map(|t| t.elapsed() >= AUTO_HIDE).unwrap_or(false),
        Err(_) => false,
    };
    if expired {
        hide();
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        // 点面板内部即关闭：不抢焦点就没法靠失焦关闭，得给个明确出口
        WM_LBUTTONDOWN => {
            hide();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    if hdc.is_invalid() {
        return;
    }

    let mut rc = RECT::default();
    if !GetClientRect(hwnd, &mut rc).is_ok() {
        let _ = EndPaint(hwnd, &ps);
        return;
    }

    // 背景：浅色（面板暂不跟随主题，深色主题是 M2 的事）
    let bg = CreateSolidBrush(rgb(250, 250, 252));
    let _ = FillRect(hdc, &rc, bg);
    let _ = DeleteObject(bg.into());

    let _ = SetBkMode(hdc, TRANSPARENT);
    // 声明 DPI 感知后系统不再替我们拉伸，字体得自己按 DPI 放大，
    // 否则 4K + 150% 上文字小得几乎看不清。用 ClearType 保证边缘不发虚。
    let s = scale();
    let font = create_font((9.0 * s).round() as i32);
    let old = SelectObject(hdc, font.into());

    // 标题
    let _ = SetTextColor(hdc, rgb(70, 70, 70));
    let title = wide("PowerRing — 设备电量");
    let _ = TextOutW(hdc, pad(), (8.0 * s).round() as i32, &title);

    if let Ok(lines) = LINES.lock() {
        for (i, (text, (r, g, b))) in lines.iter().enumerate() {
            let y = top() + i as i32 * row_h();
            // 左侧色块：一眼看出档位，比读数字快
            let sw = CreateSolidBrush(rgb(*r, *g, *b));
            let srect = RECT {
                left: pad(),
                top: y + (8.0 * s).round() as i32,
                right: pad() + (10.0 * s).round() as i32,
                bottom: y + (18.0 * s).round() as i32,
            };
            let _ = FillRect(hdc, &srect, sw);
            let _ = DeleteObject(sw.into());

            let _ = SetTextColor(hdc, rgb(40, 40, 40));
            let w = wide(text);
            let _ = TextOutW(hdc, pad() + (20.0 * s).round() as i32, y + (5.0 * s).round() as i32, &w);
        }
    }

    SelectObject(hdc, old);
    let _ = DeleteObject(font.into());
    let _ = EndPaint(hwnd, &ps);
}
