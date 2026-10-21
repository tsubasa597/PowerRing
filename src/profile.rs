//! 设备协议声明（PRD §6）
//!
//! 设计目标：**加一台设备 = 加一份 YAML，不改代码**。
//!
//! 声明只描述「事实」（VID/PID、feature 号、报文类型、取值方式），不含可执行逻辑；
//! 协议实现仍住在 src/hidpp.rs。`protocol_source` 是许可证红线字段：
//! 协议常量属事实信息可移植，但禁止复制 Solaar / HeadsetControl 等 GPL 项目源码。
//!
//! 原则（对齐 F11.3）：声明文件解析失败时跳过并告警，绝不因此让整个程序崩溃。

use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::hidpp::BatteryKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// 罗技 HID++ 2.0
    Hidpp20,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// 经接收器：需要按 slot 寻址
    Receiver,
    /// 设备直连（USB 线或自带接收器已配对为一台设备）
    Direct,
}

/// 编译后的声明：十六进制已解析成数值，可直接参与比较
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub protocol: Protocol,
    pub transport: Transport,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: Option<u16>,
    pub usage: Option<u16>,
    pub battery_feature: Option<u16>,
    pub battery_kind: BatteryKind,
    /// Receiver 模式下要探测的 slot 列表
    pub slots: Vec<u8>,
    /// 设备名 feature（用于识别与显示）
    pub name_feature: Option<u16>,
    /// 若声明了该串，读到的设备名必须包含它（不区分大小写）才算命中
    pub identify_contains: Option<String>,
    pub protocol_source: String,
    pub verification: String,
}

// ---------- YAML 形状 ----------

#[derive(Debug, Deserialize)]
struct Raw {
    name: String,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    transport: String,
    #[serde(rename = "match")]
    matches: RawMatch,
    battery: RawBattery,
    #[serde(default)]
    identify: Option<RawIdentify>,
    #[serde(default)]
    slots: Option<Vec<u8>>,
    #[serde(default)]
    protocol_source: String,
    #[serde(default)]
    verification: String,
}

#[derive(Debug, Deserialize)]
struct RawMatch {
    vendor_id: String,
    product_id: String,
    #[serde(default)]
    usage_page: Option<String>,
    #[serde(default)]
    usage: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawBattery {
    #[serde(default)]
    feature: Option<String>,
    #[serde(default)]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct RawIdentify {
    #[serde(default)]
    name_feature: Option<String>,
    #[serde(default)]
    contains: Option<String>,
}

/// 解析 "0x046D" / "046D" 这类写法
fn hex(s: &str) -> Option<u16> {
    let t = s.trim().to_ascii_lowercase();
    let t = t.strip_prefix("0x").unwrap_or(&t);
    u16::from_str_radix(t, 16).ok()
}

impl Raw {
    fn compile(self) -> Result<Profile, String> {
        let protocol = match self.protocol.trim().to_ascii_lowercase().as_str() {
            "" | "hid++2.0" | "hidpp20" | "hidpp" => Protocol::Hidpp20,
            other => return Err(format!("不支持的协议 {other}")),
        };
        let transport = match self.transport.trim().to_ascii_lowercase().as_str() {
            "" | "receiver" => Transport::Receiver,
            "direct" => Transport::Direct,
            other => return Err(format!("不支持的连接方式 {other}")),
        };
        let battery_kind = match self.battery.kind.trim().to_ascii_lowercase().as_str() {
            "" | "voltage" | "voltage-mv" | "mv" => BatteryKind::Voltage,
            "percent" | "percentage" => BatteryKind::Percent,
            other => return Err(format!("不支持的电量取值方式 {other}")),
        };
        let vid = hex(&self.matches.vendor_id).ok_or("vendor_id 不是合法十六进制")?;
        let pid = hex(&self.matches.product_id).ok_or("product_id 不是合法十六进制")?;

        let slots = match (transport, self.slots.clone()) {
            (Transport::Receiver, Some(s)) if !s.is_empty() => s,
            (Transport::Receiver, _) => (1..=6u8).collect(),
            (Transport::Direct, _) => vec![1],
        };

        let ident = self.identify.unwrap_or(RawIdentify { name_feature: None, contains: None });
        let name_feature = match ident.name_feature.as_deref() {
            Some(f) => Some(hex(f).ok_or("identify.name_feature 不是合法十六进制")?),
            None => None,
        };

        Ok(Profile {
            name: self.name,
            protocol,
            transport,
            vid,
            pid,
            usage_page: self.matches.usage_page.as_deref().and_then(hex),
            usage: self.matches.usage.as_deref().and_then(hex),
            battery_feature: self.battery.feature.as_deref().and_then(hex),
            battery_kind,
            slots,
            name_feature,
            identify_contains: ident.contains,
            protocol_source: self.protocol_source,
            verification: self.verification,
        })
    }
}

/// 按 exe 位置找到 devices 目录并加载全部声明。解析失败的会打印原因并跳过。
///
/// 为什么不能直接用相对路径「devices」：它依赖的是**当前工作目录**，
/// 而开发态和发布态的布局完全不同 ——
///   · cargo 运行时：工作目录是项目根，exe 在 target/release 下
///   · 双击 exe 时：工作目录是任意位置（资源管理器里是 target/release）
/// 只认一边，另一边必然失效，表现是「托盘起来了但一台设备都扫不到」。
/// 所以按 exe 所在目录往上找，找不着再退回工作目录。
pub fn load() -> Vec<Profile> {
    for base in candidate_bases() {
        let d = base.join("devices");
        if d.is_dir() {
            println!("[profile] 设备声明目录：{:?}", d);
            return load_from(&d);
        }
    }
    eprintln!("[profile] 没找到 devices 目录，程序将检测不到任何设备");
    Vec::new()
}

/// 从 exe 所在目录开始，逐级向上找 devices；最后是工作目录与 %APPDATA%
fn candidate_bases() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // 发布布局：exe 与 devices 平级（PowerRing/devices + PowerRing/powerring.exe）
            let mut cur = dir.to_path_buf();
            out.push(cur.clone());
            // 开发布局：exe 在 target/release 里，devices 在项目根，所以往上探几级
            for _ in 0..3 {
                match cur.parent() {
                    Some(p) => {
                        cur = p.to_path_buf();
                        out.push(cur.clone());
                    }
                    None => break,
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd);
    }
    // 以后装到系统里，声明会放在 %APPDATA%\PowerRing\devices
    if let Ok(app) = std::env::var("APPDATA") {
        out.push(PathBuf::from(app).join("PowerRing"));
    }
    out
}

fn load_from(dir: &Path) -> Vec<Profile> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        eprintln!("[profile] 设备声明目录不可用：{:?}", dir);
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
        if ext != "yaml" && ext != "yml" {
            continue;
        }
        let text = match fs::read_to_string(&p) {
            Ok(t) => t,
            Err(err) => {
                eprintln!("[profile] 读取失败，跳过 {:?}：{err}", p);
                continue;
            }
        };
        match serde_yml::from_str::<Raw>(&text) {
            Ok(raw) => match raw.compile() {
                Ok(prof) => out.push(prof),
                Err(err) => eprintln!("[profile] 声明有误，跳过 {:?}：{err}", p),
            },
            Err(err) => eprintln!("[profile] YAML 解析失败，跳过 {:?}：{err}", p),
        }
    }
    out
}
