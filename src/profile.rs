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
    /// 蓝牙标准 Battery Service（0x180F）。
    /// 与上面两类最大的区别：**不需要逆向** —— UUID 是蓝牙 SIG 定的公开标准，
    /// 声明里连报文都不用写，只要有 VID/PID 就能读
    Ble,
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
            "ble" | "bluetooth" | "ble-gatt" => Protocol::Ble,
            other => return Err(format!("不支持的协议 {other}")),
        };
        // raw 协议的报文规格是必填的：没有请求字节就无从下手，
        // 与其让它带着空表跑起来再读出 0%，不如在加载时就说清楚
        let raw = if protocol == Protocol::Raw {
            Some(self.battery.compile_spec()?)
        } else {
            None
        };
        // 蓝牙设备自身就是终点，没有「一个接收器挂多台」这回事，所以不认 receiver。
        // 声明里就算写了 receiver 也按 direct 处理 —— 写错不该变成一个空列表
        let transport = match protocol {
            Protocol::Ble => Transport::Direct,
            _ => match self.transport.trim().to_ascii_lowercase().as_str() {
                "" | "receiver" => Transport::Receiver,
                "direct" => Transport::Direct,
                other => return Err(format!("不支持的连接方式 {other}")),
            },
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
    let mut all = Vec::new();
    let mut any_dir = false;
    for base in candidate_bases() {
        let d = base.join("devices");
        if d.is_dir() {
            any_dir = true;
            info!("[profile] 设备声明目录：{:?}", d);
            all.extend(load_from(&d));
        }
    }
    if !any_dir {
        warn!("[profile] 没找到 devices 目录，程序将检测不到任何设备");
        return Vec::new();
    }
    // 同一个设备可能被两个目录同时声明：内置名单里有的，后来自动发现又写了一份到用户目录。
    // 以**先加载到的为准**（exe 旁的在前，也就是内置优先），后到的整份跳过 ——
    // 留两份会让同一台设备在面板里出现两行。
    let mut out: Vec<Profile> = Vec::new();
    for p in all {
        let dup = out
            .iter()
            .any(|q| q.vid == p.vid && q.pid == p.pid && q.protocol == p.protocol);
        if dup {
            info!(
                "[profile] {} 已在前面的目录里声明过，跳过重复的一份",
                p.name
            );
            continue;
        }
        out.push(p);
    }
    out
}

/// 自动发现到的设备落到用户目录，下次启动直接命中，不必再探测一遍。
///
/// 写在 `%APPDATA%\PowerRing\devices` 而不是 exe 旁边：程序可能装在 Program Files，
/// 那边不一定有写权限；而这份东西本来就是「这台机器上认出来的」，跟着用户走更合适。
///
/// 文件名带 VID/PID，一台一个文件，方便用户整份贴给我 —— 贴过来就是能直接合并的声明。
/// 已存在的同名文件**不覆盖**：用户可能改过名字、调过字段，自动发现不该把他的改动冲掉。
pub fn save_discovered(list: &[Profile]) -> usize {
    let Some(dir) = user_devices_dir() else {
        return 0;
    };
    if let Err(e) = fs::create_dir_all(&dir) {
        warn!("[profile] 建不了用户声明目录 {:?}：{e}", dir);
        return 0;
    }
    let mut n = 0;
    for p in list {
        let path = dir.join(format!("auto-{:04X}-{:04X}.yaml", p.vid, p.pid));
        if path.exists() {
            continue;
        }
        match fs::write(&path, to_yaml(p)) {
            Ok(()) => {
                info!("[profile] 记下新设备 {:?}", path);
                n += 1;
            }
            Err(e) => warn!("[profile] 写不进 {:?}：{e}", path),
        }
    }
    n
}

/// 用户目录：%APPDATA%\PowerRing\devices
fn user_devices_dir() -> Option<PathBuf> {
    let app = std::env::var("APPDATA").ok()?;
    if app.trim().is_empty() {
        return None;
    }
    Some(PathBuf::from(app).join("PowerRing").join("devices"))
}

/// 把一份声明写成 YAML 文本。
///
/// 手写而不走 serde 是有意的：文件里要带上「这是自动生成的、可以删」的说明，
/// 序列化出来的东西没法夹带这些话，而恰恰是这些话决定了用户敢不敢动手改。
fn to_yaml(p: &Profile) -> String {
    let name = yaml_string(&p.name);
    let protocol = match p.protocol {
        Protocol::Hidpp20 => "hid++2.0",
        Protocol::Ble => "ble",
        Protocol::Raw => "raw",
    };
    let transport = match p.transport {
        Transport::Receiver => "receiver",
        Transport::Direct => "direct",
    };
    let kind = match p.battery_kind {
        BatteryKind::Voltage => "voltage",
        BatteryKind::Percent => "percent",
    };
    let mut s = String::new();
    s.push_str("# 由 PowerRing 自动发现生成：这台设备不在内置名单里，\n");
    s.push_str("# 程序用公开标准探测到它能报电量，于是记在这里。\n");
    s.push_str("# 这个文件可以随便删，删了下次启动会重新探测一遍。\n");
    s.push_str("# 想让更多人用上它，把这个文件的内容贴到项目的 Issue 里就行。\n\n");
    s.push_str(&format!("name: {name}\n"));
    s.push_str(&format!("protocol: {protocol}\n"));
    s.push_str(&format!("transport: {transport}\n"));
    s.push_str("match:\n");
    s.push_str(&format!("  vendor_id: \"0x{:04X}\"\n", p.vid));
    s.push_str(&format!("  product_id: \"0x{:04X}\"\n", p.pid));
    if p.protocol == Protocol::Hidpp20 {
        let slots = p
            .slots
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        s.push_str(&format!("slots: [{slots}]\n"));
    }
    s.push_str("battery:\n");
    if let Some(f) = p.battery_feature {
        s.push_str(&format!("  feature: \"0x{f:04X}\"\n"));
    }
    s.push_str(&format!("  kind: {kind}\n"));
    // 这两个字段的值已经是「带引号的字符串」，别再套一层引号 ——
    // 套两层会让 YAML 解析出一个以引号开头的怪值，而且解析失败是静默跳过的，
    // 用户只会看到这台设备不见了，很难联想到是这里多写了一对引号
    s.push_str(&format!(
        "protocol_source: {}\n",
        yaml_string(&p.protocol_source)
    ));
    s.push_str(&format!("verification: {}\n", yaml_string(&p.verification)));
    if let Some(nf) = p.name_feature {
        s.push_str("identify:\n");
        s.push_str(&format!("  name_feature: \"0x{nf:04X}\"\n"));
    }
    s
}

/// YAML 双引号字符串：引号、反斜杠和控制字符都得处理，
/// 否则设备名里一个引号就能让整份声明解析失败 —— 而声明解析失败是静默跳过的，
/// 用户只会看到「这台设备没了」，很难联想到是自己名字里的引号惹的祸。
fn yaml_string(s: &str) -> String {
    let esc: String = s
        .chars()
        .map(|c| match c {
            '"' => "\\\"".to_string(),
            '\\' => "\\\\".to_string(),
            '\n' | '\r' | '\t' => ' '.to_string(),
            c if c.is_control() => String::new(),
            c => c.to_string(),
        })
        .collect();
    format!("\"{esc}\"")
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
    // 同一个目录可能从两条路被推进来（从 exe 往上找会经过它，当前工作目录也可能正好是它）。
    // 不去重的话同一份声明会被读两遍，后面的靠去重挡住，日志里平白多出几行
    // 「已在前面的目录里声明过」，看起来像用户多了几份重复声明。
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
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
