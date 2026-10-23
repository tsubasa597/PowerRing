//! 设备协议声明（PRD §6）
//!
//! 设计目标：**加一台设备 = 加一份 YAML，不改代码**。
//!
//! 声明只描述「事实」（VID/PID、feature 号、报文类型、取值方式），不含可执行逻辑；
//! 协议实现仍住在 src/hidpp.rs。`protocol_source` 是许可证红线字段：
//! 协议常量属事实信息可移植，但禁止复制 Solaar / HeadsetControl 等 GPL 项目源码。
//!
//! 原则（对齐 F11.3）：声明文件解析失败时跳过并告警，绝不因此让整个程序崩溃。

use crate::{info, warn};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::hidpp::BatteryKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// 罗技 HID++ 2.0
    Hidpp20,
    /// 原始报文：请求/响应都是裸字节，格式全写在声明里。
    /// 为非 HID++ 的设备（HyperX、雷蛇等）准备 —— 它们的报文各家自定义，
    /// 与其每加一家就写一份 Rust，不如把「发什么、读哪几个字节、怎么换算」
    /// 都变成声明里的字段
    Raw,
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
    /// 托盘悬停文字用的短名。同时挂好几台设备时全名拼起来会很长，
    /// 而这个位置只有一两秒的阅读时间，短名才扫得完。
    /// 不填就用全名 —— 完整名字始终在左键面板里，不会丢信息
    pub short_name: Option<String>,
    pub protocol: Protocol,
    pub transport: Transport,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: Option<u16>,
    pub usage: Option<u16>,
    pub battery_feature: Option<u16>,
    pub battery_kind: BatteryKind,
    /// protocol: raw 时的报文规格
    pub raw: Option<RawSpec>,
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
    short: Option<String>,
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
    // 以下仅 protocol: raw 用到
    #[serde(default)]
    request: Option<String>,
    #[serde(default)]
    reply_len: Option<usize>,
    #[serde(default)]
    state_index: Option<usize>,
    #[serde(default)]
    value_index: Option<usize>,
    #[serde(default)]
    charging_states: Option<Vec<String>>,
    /// 档位 → 分段表。key 是档位字节（如 "0x0E"），value 是 [[上界, 百分比], ...]
    #[serde(default)]
    table: Option<std::collections::HashMap<String, Vec<(u8, u8)>>>,
}

impl RawBattery {
    fn compile_spec(&self) -> Result<RawSpec, String> {
        let request = self
            .request
            .as_deref()
            .and_then(hex_bytes)
            .ok_or("raw 协议缺少 battery.request（如 \"21 FF 05\"）")?;
        let value_index = self
            .value_index
            .ok_or("raw 协议缺少 battery.value_index（数值字节的下标）")?;
        let mut table: Vec<(u8, Vec<(u8, u8)>)> = Vec::new();
        if let Some(t) = &self.table {
            for (k, rows) in t {
                let state = hex_byte(k).ok_or(format!("table 的档位键 {k} 不是合法十六进制"))?;
                if rows.is_empty() {
                    return Err(format!("table[{k}] 是空的"));
                }
                table.push((state, rows.clone()));
            }
            // 查表是顺序扫描，排序后可以让声明里的书写顺序不影响结果
            table.sort_by_key(|(s, _)| *s);
        }
        Ok(RawSpec {
            reply_len: self.reply_len.unwrap_or(20).max(request.len()),
            request,
            state_index: self.state_index.unwrap_or(value_index.saturating_sub(1)),
            value_index,
            charging_states: self
                .charging_states
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|s| hex_byte(s).ok_or(format!("charging_states 里的 {s} 不是合法十六进制")))
                .collect::<Result<Vec<u8>, String>>()?,
            table,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawIdentify {
    #[serde(default)]
    name_feature: Option<String>,
    #[serde(default)]
    contains: Option<String>,
}

/// protocol: raw 的报文规格。字段全部来自声明，代码里不出现任何厂商常量
#[derive(Debug, Clone)]
pub struct RawSpec {
    /// 请求报文（首字节是 report id），会补 0 到 reply_len 的长度
    pub request: Vec<u8>,
    /// 期望的响应长度。发请求时报文也按这个长度补齐 —— 很多设备对
    /// 长度敏感，短了干脆不回（罗技那边踩过同样的坑）
    pub reply_len: usize,
    /// 响应里「档位」字节的下标。有些设备分成几档，每档的数值含义不同
    pub state_index: usize,
    /// 响应里「数值」字节的下标
    pub value_index: usize,
    /// 档位字节取这些值时表示正在充电
    pub charging_states: Vec<u8>,
    /// 分段映射表：每个档位一串 (数值上界, 百分比)，
    /// 数值落在第一个「不超过上界」的区间里就取那个百分比。
    /// 用分段而不是公式 —— 实测这些设备的数值与百分比不是线性关系
    pub table: Vec<(u8, Vec<(u8, u8)>)>,
}

/// 解析 "0x046D" / "046D" 这类写法
fn hex(s: &str) -> Option<u16> {
    let t = s.trim().to_ascii_lowercase();
    let t = t.strip_prefix("0x").unwrap_or(&t);
    u16::from_str_radix(t, 16).ok()
}

/// 解析单个字节 "0x0E" / "0E" / "231"
fn hex_byte(s: &str) -> Option<u8> {
    let t = s.trim().to_ascii_lowercase();
    let t = t.strip_prefix("0x").unwrap_or(&t);
    u8::from_str_radix(t, 16).ok()
}

/// 解析 "21 FF 05" 这样的空格分隔字节序列
fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    let out: Option<Vec<u8>> = s.split_whitespace().map(hex_byte).collect();
    out.filter(|v| !v.is_empty())
}

impl Raw {
    fn compile(self) -> Result<Profile, String> {
        let protocol = match self.protocol.trim().to_ascii_lowercase().as_str() {
            "" | "hid++2.0" | "hidpp20" | "hidpp" => Protocol::Hidpp20,
            "raw" | "raw_report" => Protocol::Raw,
            other => return Err(format!("不支持的协议 {other}")),
        };
        // raw 协议的报文规格是必填的：没有请求字节就无从下手，
        // 与其让它带着空表跑起来再读出 0%，不如在加载时就说清楚
        let raw = if protocol == Protocol::Raw {
            Some(self.battery.compile_spec()?)
        } else {
            None
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

        let ident = self.identify.unwrap_or(RawIdentify {
            name_feature: None,
            contains: None,
        });
        let name_feature = match ident.name_feature.as_deref() {
            Some(f) => Some(hex(f).ok_or("identify.name_feature 不是合法十六进制")?),
            None => None,
        };

        // 短名留空就用全名，但去掉首尾空白：YAML 里顺手写的空格不该进到界面上
        let short_name = self
            .short
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        Ok(Profile {
            name: self.name,
            short_name,
            protocol,
            transport,
            vid,
            pid,
            usage_page: self.matches.usage_page.as_deref().and_then(hex),
            usage: self.matches.usage.as_deref().and_then(hex),
            battery_feature: self.battery.feature.as_deref().and_then(hex),
            battery_kind,
            raw,
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
            info!("[profile] 设备声明目录：{:?}", d);
            return load_from(&d);
        }
    }
    warn!("[profile] 没找到 devices 目录，程序将检测不到任何设备");
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
        warn!("[profile] 设备声明目录不可用：{:?}", dir);
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
                warn!("[profile] 读取失败，跳过 {:?}：{err}", p);
                continue;
            }
        };
        match serde_yml::from_str::<Raw>(&text) {
            Ok(raw) => match raw.compile() {
                Ok(prof) => out.push(prof),
                Err(err) => warn!("[profile] 声明有误，跳过 {:?}：{err}", p),
            },
            Err(err) => warn!("[profile] YAML 解析失败，跳过 {:?}：{err}", p),
        }
    }
    out
}
