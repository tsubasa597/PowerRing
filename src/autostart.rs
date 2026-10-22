//! 开机自启：往 HKCU 的 Run 键里写一行
//!
//! 用 HKCU 而不是 HKLM：HKLM 要管理员权限，为一个开机才跑一次的托盘小工具弹 UAC 不值。
//! HKCU 只对当前用户生效，而「我的电脑开机就显示我的鼠标电量」本来就是用户级的事。
//!
//! **真相来源是注册表本身，不是 config.json。**
//! 用户可能在任务管理器的「启动」页里禁用它（那份开关存在另一个键里，config 读不到），
//! 也可能手动删掉这一行。所以每次打开设置都重新问一次注册表，显示的是真实状态；
//! config 里那份只用作「上次用户想不想自启」的意图，启动时拿它做一次自检。

use crate::{info, warn};
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPEN_CREATE_OPTIONS, REG_SAM_FLAGS, REG_SZ,
};

/// Run 键是 Windows 登录时挨个启动程序的地方，也是任务管理器「启动」页读的那一个
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "PowerRing";

// 值不存在时删它是正常的（用户已经手动删过），不算失败
// WIN32_ERROR 的字段是 u32，跟着它走省得来回转类型
const ERROR_FILE_NOT_FOUND: u32 = 2;

fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 当前 exe 的完整路径。
/// 用 current_exe() 而不是 argv[0]：自启时命令行可能带别的参数或工作目录，
/// 而 current_exe 永远是磁盘上那个文件本身。
fn exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// 写进注册表的值。路径带引号 —— 装到「Program Files」这类带空格的目录下时，
/// 不带引号会被 Shell 当成两段：「C:\Program」+ 一个叫 Files\... 的参数。
fn command_line() -> Option<Vec<u16>> {
    let p = exe_path()?;
    Some(wide_nul(&format!("\"{}\"", p.display())))
}

/// 打开（不存在则创建）Run 键。返回的句柄用完要 RegCloseKey。
fn open_key() -> Option<HKEY> {
    let sub = wide_nul(RUN_KEY);
    let mut hk = HKEY::default();
    let err = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPEN_CREATE_OPTIONS(0),
            REG_SAM_FLAGS(KEY_READ.0 | KEY_WRITE.0),
            None,
            &mut hk,
            None,
        )
    };
    if err.0 != 0 {
        warn!("[autostart] 打不开 Run 键：错误 {}", err.0);
        return None;
    }
    Some(hk)
}

/// 读出注册表里那一行。不存在返回 None（这跟「存在但为空」不同，后者也算没设）
fn read_value() -> Option<String> {
    let hk = open_key()?;
    let name = wide_nul(VALUE_NAME);
    unsafe {
        // 第一趟只要长度：缓冲区大小只有问过才知道
        let mut size: u32 = 0;
        let probe = RegQueryValueExW(hk, PCWSTR(name.as_ptr()), None, None, None, Some(&mut size));
        if probe.0 != 0 || size == 0 {
            let _ = RegCloseKey(hk);
            return None;
        }
        // REG_SZ 是 UTF-16，按 u16 个数申请，末尾的 NUL 也算在 size 里
        let mut buf: Vec<u16> = vec![0; (size as usize + 1) / 2];
        let mut got = size;
        let mut ty = REG_SZ;
        let err = RegQueryValueExW(
            hk,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut got),
        );
        let _ = RegCloseKey(hk);
        if err.0 != 0 {
            return None;
        }
        let n = (got as usize) / 2;
        if n == 0 {
            return None;
        }
        // 结尾的 NUL 不是内容，去掉
        let text = String::from_utf16_lossy(&buf[..n.min(buf.len())])
            .trim_end_matches('\0')
            .to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

/// 当前这份 exe 有没有登记开机自启。
///
/// 不只看「值存不存在」，还要看它指向的是不是**当前这个 exe** ——
/// 用户把程序挪了地方（比如从 target\debug 换成 release），旧路径就指向一个不存在的文件，
/// 那时显示「已开启」是骗人。
pub fn is_enabled() -> bool {
    let cur = match exe_path() {
        Some(p) => p,
        None => return false,
    };
    match read_value() {
        Some(v) => {
            // 允许值里带引号和启动参数，取第一段路径来比
            let first = v.trim().trim_matches('"');
            let hit = first.eq_ignore_ascii_case(&cur.to_string_lossy().to_string());
            info!(
                "[autostart] 注册表里是 {:?}，当前 exe 是 {:?}，判定为 {}",
                v,
                cur.display(),
                if hit { "已开启" } else { "未开启（指向别处）" }
            );
            hit
        }
        None => {
            info!("[autostart] 注册表里没有 PowerRing，判定为未开启");
            false
        }
    }
}

/// 打开或关闭开机自启。
///
/// 失败只回报不 panic：这是锦上添花的功能，注册表被策略挡住（公司电脑常见）
/// 也不能连累电量监测本身。
pub fn set(on: bool) -> Result<(), String> {
    let hk = open_key().ok_or_else(|| "打不开注册表的 Run 键".to_string())?;
    let name = wide_nul(VALUE_NAME);

    let err = if on {
        let cmd = command_line().ok_or_else(|| "取不到本程序的路径".to_string())?;
        let bytes: Vec<u8> = cmd
            .iter()
            .flat_map(|u| u.to_ne_bytes())
            .collect();
        unsafe {
            RegSetValueExW(
                hk,
                PCWSTR(name.as_ptr()),
                None,
                REG_SZ,
                Some(&bytes),
            )
        }
    } else {
        unsafe { RegDeleteValueW(hk, PCWSTR(name.as_ptr())) }
    };
    unsafe {
        let _ = RegCloseKey(hk);
    }

    // 关的时候值本来就不存在，等于已经是想要的状态，不算错
    if err.0 == 0 || (!on && err.0 == ERROR_FILE_NOT_FOUND) {
        info!("[autostart] 开机自启已{}", if on { "开启" } else { "关闭" });
        Ok(())
    } else {
        let msg = format!("写注册表失败（错误 {}）", err.0);
        warn!("[autostart] {}", msg);
        Err(msg)
    }
}

/// 启动自检：用户想自启，但注册表里没有（或被别的东西改掉了），补回去。
///
/// 什么时候会走到这：程序挪了位置、清理软件删过启动项、config 是拷贝来的。
/// 反过来不成立 —— 注册表里有而 config 说不要，说明是用户手动加的，不去动它。
pub fn repair(expected: bool) {
    if expected && !is_enabled() {
        info!("[autostart] 配置要求自启但注册表里没有，补写一次");
        if let Err(e) = set(true) {
            warn!("[autostart] 补写失败：{}", e);
        }
    }
}
