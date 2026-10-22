//! 系统通知（Toast），进 Windows 通知中心
//!
//! 自绘卡片（popup.rs）的优势是能精确贴在托盘图标上方、能跟主题配色；
//! 但它是「自己的窗口」，有两个绕不过去的短板：
//!   · 被别的东西一盖就没了，之后再也找不回来（通知中心里永远有历史）；
//!   · 不吃系统的免打扰/专注助手，也不跟随用户在「通知和操作」里的设置。
//! 所以再补一条系统通知的路子，让用户在设置里挑。
//!
//! Win10 上想让通知在通知中心留痕，必须同时满足两件事：
//!   1. 进程声明 AppUserModelID（SetCurrentProcessExplicitAppUserModelID）
//!   2. 开始菜单里有一个带**同样** AppUserModelID 的快捷方式
//! 少任何一件，通知就只是「弹一下就没」，关掉之后再也找不回来。
//! 两件里第二件要写文件、可能因权限失败，所以全部做成 best effort：
//! 失败就退回自绘卡片，绝不因此让程序起不来。
//!
//! 另一个刻意的设计：CoInitializeEx 用 STA（COINIT_APARTMENTTHREADED）。
//! 调它的就是跑消息泵的那个主线程，本来就该是 STA；WinRT 的 Toast 也要求
//! 线程先进公寓。别在轮询线程上调 —— 那条线程只碰 HID。

use crate::{info, warn};
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{HSTRING, Interface, PCWSTR, PWSTR};
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    StructuredStorage::PROPVARIANT, IPersistFile,
};
use windows::Win32::System::Variant::VARENUM;
use windows::Win32::UI::Shell::{SetCurrentProcessExplicitAppUserModelID, ShellLink, IShellLinkW};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

/// 应用标识。通知中心靠它认人，改了名字旧的快捷方式就对不上了 ——
/// 真要改就得让 ensure_shortcut 重建一次
const APP_ID: &str = "PowerRing.PowerRing";

/// PKEY_AppUserModel_ID = {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5
/// crate 里它挂在 Win32_Storage_EnhancedStorage 下，为一个常量去开那个 feature 不值，
/// 这个 GUID 是公开的、不会变，照抄一份是安全的
const PKEY_APP_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
    pid: 5,
};
/// VT_LPWSTR
const VT_LPWSTR: u16 = 31;

static READY: AtomicBool = AtomicBool::new(false);

/// 通知是否可用。设置窗口据此决定要不要提示「退回卡片」
pub fn available() -> bool {
    READY.load(Ordering::SeqCst)
}

/// 启动时调一次：声明 AppUserModelID 并确保开始菜单有快捷方式。
/// 返回 false 时 notify::show 也多半会失败，调用方应退回自绘卡片。
pub fn init() -> bool {
    match set_app_id() {
        Ok(()) => {}
        Err(e) => {
            warn!("[notify] 声明 AppUserModelID 失败：{e}");
            return false;
        }
    }
    match ensure_shortcut() {
        Ok(()) => {}
        Err(e) => {
            // 快捷方式建不出来不等于通知弹不出来（只是不会在通知中心留痕），
            // 所以这里不判死刑，记下来继续
            warn!("[notify] 开始菜单快捷方式未能建立：{e}（通知可能不进通知中心）");
        }
    }
    READY.store(true, Ordering::SeqCst);
    info!("[notify] 系统通知已就绪（应用标识 {}）", APP_ID);
    true
}

fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn set_app_id() -> windows::core::Result<()> {
    let id = wide_nul(APP_ID);
    unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(id.as_ptr())) }
}

/// 确保 `%APPDATA%\Microsoft\Windows\Start Menu\Programs\PowerRing.lnk` 存在。
///
/// 已经存在就直接返回 —— 用户可能自己改过这个快捷方式（换图标、加参数），
/// 每次启动都重写一遍是在抹他的设置。
fn ensure_shortcut() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("取不到 exe 路径：{e}"))?;
    let appdata = std::env::var("APPDATA").map_err(|e| format!("取不到 APPDATA：{e}"))?;
    let mut path = std::path::PathBuf::from(appdata);
    path.push("Microsoft");
    path.push("Windows");
    path.push("Start Menu");
    path.push("Programs");
    path.push("PowerRing.lnk");
    if path.exists() {
        return Ok(());
    }

    let exe_w = wide_nul(&exe.to_string_lossy());
    let lnk_w = wide_nul(&path.to_string_lossy());
    let id_w = wide_nul(APP_ID);
    let desc_w = wide_nul("无线外设电量监测");

    unsafe {
        // 已经初始化过会返回 RPC_E_CHANGED_MODE，那种情况照用不误；
        // 这里只看 CoCreateInstance 成不成，CoInitialize 的返回值不作为判据
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        link.SetPath(PCWSTR(exe_w.as_ptr())).map_err(|e| e.to_string())?;
        link.SetDescription(PCWSTR(desc_w.as_ptr()))
            .map_err(|e| e.to_string())?;

        // AUMID 得写进快捷方式的属性里：通知中心是拿这个属性把通知归档到应用名下，
        // 光有进程级的声明，通知会归档到「未知应用」或者干脆不留痕
        let store: IPropertyStore = link.cast::<IPropertyStore>().map_err(|e| e.to_string())?;
        // PROPVARIANT 是个三层嵌套的 union，直接赋值会触发「写引用会析构旧值」
        // 的检查（union 字段的旧值是什么类型并不确定，编译器不敢动它）。
        // 用 ptr::write 绕开，这也是官方示例里写 PROPVARIANT 的常规做法
        let mut pv = PROPVARIANT::default();
        let body = &mut pv.Anonymous.Anonymous;
        std::ptr::write(&mut body.vt, VARENUM(VT_LPWSTR));
        std::ptr::write(
            &mut body.Anonymous.pwszVal,
            PWSTR(id_w.as_ptr() as *mut u16),
        );
        store
            .SetValue(&PKEY_APP_ID, &pv)
            .map_err(|e| e.to_string())?;
        store.Commit().map_err(|e| e.to_string())?;

        let pf: IPersistFile = link.cast::<IPersistFile>().map_err(|e| e.to_string())?;
        pf.Save(PCWSTR(lnk_w.as_ptr()), true)
            .map_err(|e| e.to_string())?;
    }
    info!("[notify] 已建开始菜单快捷方式：{}", path.display());
    Ok(())
}

/// 发一条系统通知。失败原因写在返回值里，调用方据此决定要不要退回自绘卡片。
pub fn show(title: &str, lines: &[String]) -> Result<(), String> {
    let doc = XmlDocument::new().map_err(|e| e.to_string())?;
    doc.LoadXml(&HSTRING::from(build_xml(title, lines)))
        .map_err(|e| e.to_string())?;
    let toast = ToastNotification::CreateToastNotification(&doc).map_err(|e| e.to_string())?;
    // 用带 ID 的版本：进程级 AUMID 已经声明过，但显式指定一遍更稳，
    // 也避免别的组件改过进程 AUMID 之后通知发到别人名下
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
        .map_err(|e| e.to_string())?;
    notifier.Show(&toast).map_err(|e| e.to_string())
}

/// 拼 toast 的 XML。ToastGeneric 模板：第一行加粗当标题，后面几行是正文。
/// 只取前 3 行 —— 通知框就那么大，多出来的是被折叠的，不如提前截断
fn build_xml(title: &str, lines: &[String]) -> String {
    let mut s = String::from("<toast><visual><binding template=\"ToastGeneric\">");
    s.push_str(&format!("<text>{}</text>", esc(title)));
    for l in lines.iter().take(3) {
        s.push_str(&format!("<text>{}</text>", esc(l)));
    }
    if lines.len() > 3 {
        s.push_str(&format!("<text>还有 {} 台…</text>", lines.len() - 3));
    }
    // 静音：电量提醒不该响。不写 audio 节点会用默认提示音
    s.push_str("</binding></visual><audio silent=\"true\"/></toast>");
    s
}

/// XML 转义。设备名是从 HID 报文里读出来的，理论上可能带 & < >，
/// 不转义会让整条 XML 解析失败 —— 表现就是点了「试一次」什么都没发生
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
