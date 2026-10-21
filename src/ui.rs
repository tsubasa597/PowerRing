//! 界面共用的小工具：DPI 缩放、字体、颜色、屏幕范围
//!
//! 抽出来的原因很实在：托盘面板和设置窗口都要按 DPI 放大、都要自建字体，
//! 不抽就得在两处各写一遍 —— 而这两处一旦不一致，界面就会一半大一半小。
//! 仍然不引 GUI 框架（ADR-001），这里只有几十行 Win32 调用。

use windows::core::w;
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, FF_DONTCARE, HFONT,
    OUT_DEFAULT_PRECIS,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SYSTEM_METRICS_INDEX};

// 下面四个 SM_* 是虚拟桌面（所有显示器拼起来的大矩形）的位置与尺寸。
// 直接写数值而不是引常量：它们在 windows crate 里散落各处，为四个数多开依赖不值。
// 用虚拟桌面而不是主屏，是因为用户是双屏，按主屏算会把窗口从副屏拽回来。
const SM_XVIRTUALSCREEN: i32 = 76;
const SM_YVIRTUALSCREEN: i32 = 77;
const SM_CXVIRTUALSCREEN: i32 = 78;
const SM_CYVIRTUALSCREEN: i32 = 79;

/// 96 DPI = 1.0；4K + 150% 缩放 = 1.5
pub fn scale() -> f32 {
    let dpi = unsafe { GetDpiForSystem() };
    if dpi <= 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

/// 把 96 DPI 下的设计尺寸换算成当前 DPI 的像素
pub fn px(base: f32) -> i32 {
    (base * scale()).round() as i32
}

/// 字号（磅）→ CreateFontW 要的像素高度。
///
/// 磅是物理单位（1pt = 1/72 英寸），CreateFontW 收的是像素，两者必须换算：
/// 96 DPI 下 1px = 1/96 英寸，所以 px = pt × 96/72。再按 DPI 放大成物理像素。
/// 直接把 9 当像素传出去，在 144 DPI 上只有 6.75pt —— 小得离谱，
/// 这个坑踩过一次：当时以为 9 就是 9 磅。
pub fn font_px(pt: f32) -> i32 {
    (pt * 96.0 / 72.0 * scale()).round() as i32
}

/// 造一个指定像素高度的字体。声明 DPI 感知后系统不再替我们拉伸界面，
/// 字体必须自己按 DPI 放大，否则 4K 上小得看不清。
/// 高度传负值 = 按字符本身高度算（不含内部行距），排版更好控。
pub fn create_font(px: i32) -> HFONT {
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

pub fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// 虚拟桌面的 (x, y, 宽, 高)，用来把窗口夹在可见区域内
pub fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SYSTEM_METRICS_INDEX(SM_XVIRTUALSCREEN)),
            GetSystemMetrics(SYSTEM_METRICS_INDEX(SM_YVIRTUALSCREEN)),
            GetSystemMetrics(SYSTEM_METRICS_INDEX(SM_CXVIRTUALSCREEN)),
            GetSystemMetrics(SYSTEM_METRICS_INDEX(SM_CYVIRTUALSCREEN)),
        )
    }
}
