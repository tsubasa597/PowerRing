//! 设备扫描与读取：把「声明」变成「活的设备」
//!
//! 一台逻辑设备 = 一份声明 + 一条实际链路（如接收器的某个 slot）。
//! 扫描只依赖声明里的 VID/PID/usage，不硬编码任何厂商常量。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hidapi::HidApi;

use crate::hidpp::{Battery, BatteryKind, Hidpp};
use crate::profile::{Profile, Protocol, Transport};
use crate::raw::RawDev;

/// 一条实际链路。feature / kind / 报文格式都来自声明 ——
/// 读什么、怎么解读，不再由代码硬编码。
enum Link {
    Hidpp {
        conn: Arc<Hidpp>,
        dev_idx: u8,
        feature: u16,
        kind: BatteryKind,
    },
    Raw {
        dev: Arc<RawDev>,
    },
}

pub struct Device {
    /// 显示名：优先用设备自报的名字，取不到才用声明名
    pub name: String,
    link: Link,
}

impl Device {
    pub fn read(&self) -> Option<Battery> {
        match &self.link {
            Link::Hidpp { conn, dev_idx, feature, kind } => match kind {
                BatteryKind::Voltage => conn.battery_voltage(*dev_idx, *feature),
                BatteryKind::Percent => conn.battery_percent(*dev_idx, *feature),
            },
            Link::Raw { dev } => dev.battery(),
        }
    }
}

/// 按声明扫描本机所有受支持设备
pub fn scan(api: &HidApi, profiles: &[Profile]) -> Vec<Device> {
    // 同一个 HID 接口可能被多份声明引用（一个接收器上挂着多台设备），只打开一次
    let mut opened: HashMap<String, Arc<Hidpp>> = HashMap::new();
    // raw 设备按「接口 + 声明」缓存：一份声明对应一种报文格式，不能共用
    let mut raws: HashMap<String, Arc<RawDev>> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Device> = Vec::new();

    for prof in profiles {
        for info in api.device_list() {
            if info.vendor_id() != prof.vid || info.product_id() != prof.pid {
                continue;
            }
            if let Some(up) = prof.usage_page {
                if info.usage_page() != up {
                    continue;
                }
            }
            if let Some(u) = prof.usage {
                if info.usage() != u {
                    continue;
                }
            }

            let key = info.path().to_string_lossy().to_string();

            match prof.protocol {
                Protocol::Hidpp20 => {
                    let conn = match opened.get(&key) {
                        Some(c) => c.clone(),
                        None => {
                            let Ok(dev) = info.open_device(api) else {
                                continue;
                            };
                            let conn = Arc::new(Hidpp::new(dev));
                            opened.insert(key.clone(), conn.clone());
                            conn
                        }
                    };
                    scan_hidpp(&conn, prof, &key, &mut seen, &mut out);
                }
                Protocol::Raw => {
                    let Some(spec) = prof.raw.clone() else {
                        continue;
                    };
                    let rkey = format!("{key}#{}", prof.name);
                    let dev = match raws.get(&rkey) {
                        Some(d) => d.clone(),
                        None => {
                            let Ok(h) = info.open_device(api) else {
                                continue;
                            };
                            let d = Arc::new(RawDev::new(h, spec));
                            raws.insert(rkey.clone(), d.clone());
                            d
                        }
                    };
                    scan_raw(&dev, prof, &rkey, &mut seen, &mut out);
                }
            }
        }
    }
    out
}

/// raw 设备的扫描：**能读到一条有效响应才算它在**。
///
/// 只比对 VID/PID 是不够的 —— 无线接收器是插在机器上的，耳机开机与否
/// 枚举结果完全一样。靠一次探测请求区分：耳机没开就没有响应，
/// 于是它自然从列表里消失，而不是显示一个假的 0%。
fn scan_raw(
    dev: &Arc<RawDev>,
    prof: &Profile,
    key: &str,
    seen: &mut HashSet<String>,
    out: &mut Vec<Device>,
) {
    // 探测失败**不**记进 seen：这一轮没开机，下一轮还可能开机，别把它永久划掉
    if !dev.probe() {
        return;
    }
    if !seen.insert(key.to_string()) {
        return;
    }
    out.push(Device {
        name: prof.name.clone(),
        link: Link::Raw { dev: dev.clone() },
    });
}

fn scan_hidpp(
    conn: &Arc<Hidpp>,
    prof: &Profile,
    path_key: &str,
    seen: &mut HashSet<String>,
    out: &mut Vec<Device>,
) {
    // 该 slot 上有没有设备，靠「电量 feature 是否存在」判断 —— M0 实测 ping 不可靠
    let Some(feat) = prof.battery_feature else {
        return;
    };
    let idxs: Vec<u8> = match prof.transport {
        Transport::Receiver => prof.slots.clone(),
        Transport::Direct => vec![1],
    };

    for dev_idx in idxs {
        if conn.feature_index(dev_idx, feat).is_none() {
            continue;
        }
        let uniq = format!("{path_key}#{dev_idx}");
        if !seen.insert(uniq) {
            continue;
        }

        // 识别：读设备名并与声明里的关键字比对，避免同接收器上的其它设备被误认
        let reported = prof
            .name_feature
            .and_then(|f| conn.feature_index(dev_idx, f))
            .and_then(|i| conn.device_name_at(dev_idx, i));
        if let Some(want) = &prof.identify_contains {
            let hit = reported
                .as_deref()
                .map(|n| n.to_ascii_lowercase().contains(&want.to_ascii_lowercase()))
                .unwrap_or(false);
            if !hit {
                continue;
            }
        }

        let name = reported
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| prof.name.clone());

        out.push(Device {
            name,
            link: Link::Hidpp {
                conn: conn.clone(),
                dev_idx,
                feature: feat,
                kind: prof.battery_kind,
            },
        });
    }
}
