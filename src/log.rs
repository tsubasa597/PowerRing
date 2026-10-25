//! 日志落盘：%APPDATA%\PowerRing\log.txt，超过 1MB 滚动一次（旧档留一份）
//!
//! 存在的理由不是「记着玩」，而是为了让主程序能彻底告别控制台窗口：
//! 没有日志，一旦把子系统切成 windows（无控制台），出问题就两眼一抹黑 ——
//! 用户既看不到输出，我们也拿不到任何现场。所以先有日志，再关窗口。
//!
//! 刻意不引 log / tracing / chrono：
//!   · log + tracing 为了这点输出要拖进一套门面与宏展开，编译时间不划算
//!   · chrono 为了格式化时间，不如 30 行 civil-from-days 算法来得轻
//! 代价是自己算时间：固定按东八区显示（本项目面向国内用户，且中国无夏令时）。

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 单个日志文件上限。超过就滚一次：log.txt → log.1.txt，旧的 log.1.txt 直接丢
const MAX_BYTES: u64 = 1024 * 1024;

static WRITER: Mutex<Option<fs::File>> = Mutex::new(None);

/// 打开日志文件。失败时不可用就用「不记录」兜底 ——
/// 日志是为了排查问题，绝不能反过来把程序拖垮（PRD F11 声明解析失败同理）。
pub fn init() {
    let dir = appdata_dir();
    if let Err(err) = fs::create_dir_all(&dir) {
        // 这里还处在进程早期，控制台可能还在，先直接说出来
        eprintln!("[log] 无法创建目录 {:?}：{}", dir, err);
        return;
    }
    *WRITER.lock().unwrap_or_else(|e| e.into_inner()) = open(&dir.join("log.txt"));
    // 这里用 line 而不是 info! 宏：宏定义在文件下方，文本顺序上还没出生
    line("INFO", "=== PowerRing 启动 ===".to_string());
}

pub fn line(level: &str, msg: String) {
    let mut g = match WRITER.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    // 写之前先看体积：只在超限时才动文件，平时不付出 rename 的代价
    let too_big = g
        .as_ref()
        .and_then(|f| f.metadata().ok())
        .map(|m| m.len() >= MAX_BYTES)
        .unwrap_or(false);
    if too_big {
        *g = None; // 先关句柄，Windows 上不关就改不了名
        let old = appdata_dir().join("log.1.txt");
        let _ = fs::remove_file(&old);
        let _ = fs::rename(appdata_dir().join("log.txt"), &old);
        *g = open(&appdata_dir().join("log.txt"));
    }
    if let Some(f) = g.as_mut() {
        // 每条都 flush：日志频率很低，但崩溃时最后几行最值钱，不能不落盘
        let _ = writeln!(f, "{} [{}] {}", stamp(), level, msg);
        let _ = f.flush();
    }
}

/// 比 INFO 低一级：留给「每隔一轮就会发生一次」的那种正常状态。
/// 用 INFO 记它们会把真正值得看的行冲掉 —— 设备睡一夜就是上千条。
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log::line("DEBUG", format!($($arg)*)) };
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::line("INFO", format!($($arg)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::line("WARN", format!($($arg)*)) };
}

fn open(path: &std::path::Path) -> Option<fs::File> {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

fn appdata_dir() -> PathBuf {
    // APPDATA 取不到就退到当前目录，日志少一点总比没有强
    match std::env::var("APPDATA") {
        Ok(v) => PathBuf::from(v).join("PowerRing"),
        Err(_) => PathBuf::from("."),
    }
}

/// 当前本地时间，固定东八区。不引 chrono，自己算日历。
fn stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        + 8 * 3600;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m,
        d,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 civil_from_days：把「距 1970-01-01 的天数」转成年月日
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
