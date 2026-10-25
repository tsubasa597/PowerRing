//! 自动发现：没有声明的设备，用**公开标准**试着认一认
//!
//! 为什么需要它：声明目录里只有几台设备，一个陌生人第一次打开几乎必然是空壳。
//! 但「读电量」这件事在两条路上是公开标准，不需要任何人来逆向：
//!   · 蓝牙 Battery Service（0x180F / 0x2A19）—— 蓝牙 SIG 定的，设备自己报百分比
//!   · HID++ 2.0 的电量 feature —— 罗技自家标准，feature 号可以问出来
//! 所以与其等用户报 VID/PID 过来，不如让程序自己问一遍。
//!
//! 三条纪律（改这里前先看）：
//!   1. **必须真的读到一次合理的电量**才算发现。光答应探测不算 ——
//!      应答了却读出 0 mV 的设备写进声明，就是给用户看一个错数字
//!   2. 只对走这两条公开标准的设备动手，**不猜私有报文**。猜不出来就交给用户上报
//!   3. 探测要发报文，会唤醒睡着的设备，所以调用频率由调用方控制（见 main.rs）：
//!      启动第一轮无条件试一次，之后只在「一台设备都没有」时定期重试

use hidapi::HidApi;

use crate::hidpp::{
    Battery, BatteryKind, Hidpp, F_BATTERY_VOLTAGE, F_DEVICE_NAME, F_UNIFIED_BATTERY,
};
use crate::profile::{Profile, Protocol, Transport};
use crate::{info, warn};

/// 会答 HID++ 的厂商。HID++ 是罗技的协议，只对它家设备发这类报文 ——
/// 对着别人的设备乱发私有请求，轻则没反应，重则把正在休眠的设备叫醒。
/// 这条清单只增不减：加一个厂商意味着确认它真的实现了 HID++ 2.0。
pub(crate) const HIDPP_VENDORS: [u16; 1] = [0x046D];

/// 候选电量 feature，按「老设备多、新设备准」排序：
/// 0x1001 电压式（罗技老设备普遍支持，读出来是毫伏，走共用放电曲线换算）
/// 0x1004 统一电量式（较新的设备，直接给百分比和充电状态）
/// 两者都是罗技公开文档写明编号的 feature，不是猜出来的。
const BATTERY_FEATURES: [(u16, BatteryKind); 2] = [
    (F_BATTERY_VOLTAGE, BatteryKind::Voltage),
    (F_UNIFIED_BATTERY, BatteryKind::Percent),
];

/// 直连设备（插线、或接收器已与设备配对成一台）用的 device index。
/// M2 实测：直连时不挑 index，0x00–0xFF 全都响应，用 1 即可。
pub(crate) const DIRECT_IDX: u8 = 1;
/// 用来分辨接收器与直连设备的那个 index：故意取一个不是 1 的值，
/// 只有「不挑 index 的直连设备」才会理它（见 `looks_direct`）
const PROBE_IDX: u8 = 2;
/// 接收器上要逐个问的 slot。slot 会漂移（实测 GPW 从 3 漂到 2），所以全覆盖
const RECEIVER_SLOTS: std::ops::RangeInclusive<u8> = 1..=6;

/// 电压式读数的合理区间（毫伏）。
///
/// 一节锂电是 3.0–4.2 V，两串是 6.0–8.4 V，这里按单节取宽松边界。
/// 作用是把「应答了但读出一堆零」挡在外面 —— 那种设备写进声明就是错的。
/// 区间放得很宽是有意的：窄一点就可能把某个型号的低电量状态判成无效。
const PLAUSIBLE_MV: std::ops::RangeInclusive<u16> = 2500..=5000;

/// 这个读数能不能信。
///
/// 百分比式没有可校验的东西（0..100 本来就全覆盖），只有电压式能看数值像不像电压。
fn plausible(b: &Battery, kind: BatteryKind) -> bool {
    match kind {
        BatteryKind::Voltage => PLAUSIBLE_MV.contains(&b.mv),
        BatteryKind::Percent => true,
    }
}

/// 按 kind 读一次电量
fn read_battery(conn: &Hidpp, idx: u8, feature: u16, kind: BatteryKind) -> Option<Battery> {
    match kind {
        BatteryKind::Voltage => conn.battery_voltage(idx, feature),
        BatteryKind::Percent => conn.battery_percent(idx, feature),
    }
}

/// 试认这台设备走的是不是「直连的 HID++ 设备」
pub(crate) fn try_direct(conn: &Hidpp) -> Option<(u16, BatteryKind)> {
    for (feat, kind) in BATTERY_FEATURES {
        // feature_index 问的是「你支持这个 feature 吗」，答了才算候选
        if conn.feature_index(DIRECT_IDX, feat).is_none() {
            continue;
        }
        if let Some(b) = read_battery(conn, DIRECT_IDX, feat, kind) {
            if plausible(&b, kind) {
                return Some((feat, kind));
            }
        }
    }
    None
}

/// 试认这个接口是不是「接收器」，上面挂着几台设备
pub(crate) fn try_receiver(conn: &Hidpp) -> Option<(u8, u16, BatteryKind)> {
    for slot in RECEIVER_SLOTS {
        for (feat, kind) in BATTERY_FEATURES {
            if conn.feature_index(slot, feat).is_none() {
                continue;
            }
            if let Some(b) = read_battery(conn, slot, feat, kind) {
                if plausible(&b, kind) {
                    return Some((slot, feat, kind));
                }
            }
        }
    }
    None
}

/// 这个接口是**接收器**还是**直连设备**？
///
/// 判据：直连设备对每个 device index 都应答（M2 实测 0x00–0xFF 全响应，
/// 回显请求里的 index），而接收器只对**已配对**的 slot 应答。
/// 于是拿一个不是 1 的 index 去问：它答应，说明这个接口不挑 index，是直连。
///
/// 分错的代价并不对称：接收器被当成直连，就只按 index 1 读 —— 设备漂到别的 slot
/// 就再也读不到（实测 GPW 的 slot 会漂）；直连被当成接收器，只是多问几个空 slot，
/// 慢一点但结果一样。所以拿不准时宁可判成接收器。
pub(crate) fn looks_direct(conn: &Hidpp) -> bool {
    BATTERY_FEATURES
        .iter()
        .any(|(f, _)| conn.feature_index(PROBE_IDX, *f).is_some())
}

/// 显示名：**设备自报的**那个优先。
///
/// 接收器的 USB 产品串一律叫 "USB Receiver"，用户在意的是挂在它上面的那台鼠标叫什麼。
/// 设备名走 HID++ 的 device name feature（0x0005），读不到才退回产品串。
pub(crate) fn best_name(conn: &Hidpp, idx: u8, fallback: String) -> String {
    match conn.device_name(idx) {
        Some(n) if !n.is_empty() => n,
        _ => fallback,
    }
}

/// 一份声明里已经写过的 VID/PID：别去重复发现，用户手动加的声明优先
fn declared_key(prof: &Profile) -> (u16, u16) {
    (prof.vid, prof.pid)
}

/// 扫一遍本机，找出「没写进声明、但能用公开标准读到电量」的设备。
///
/// 返回的是可以直接用的声明（还没落盘），由调用方决定要不要写进用户目录。
/// 打不开的接口一律跳过：系统占着不放、权限不足都会失败，
/// 那是「这台暂时碰不到」，不是错误 —— 与其报错，不如安静地跳过。
pub fn find(api: &HidApi, declared: &[Profile]) -> Vec<Profile> {
    let known: Vec<(u16, u16)> = declared.iter().map(declared_key).collect();
    let mut out = Vec::new();
    let mut seen: Vec<(u16, u16)> = Vec::new();

    // ---- 蓝牙：能枚举到就已经是「暴露了标准电池服务」的设备 ----
    // ble::enumerate() 用的是系统按 Battery Service UUID 过滤的选择器，
    // 所以这里不需要再挑一遍「它有没有电池服务」，只要没被声明覆盖过就值得一试。
    for node in crate::ble::enumerate().unwrap_or_default() {
        if known.contains(&(node.vid, node.pid)) || seen.contains(&(node.vid, node.pid)) {
            continue;
        }
        let dev = match crate::ble::Dev::open(&node.id) {
            Ok(d) => d,
            Err(e) => {
                warn!("[discover] 蓝牙设备 {} 打开失败：{e}", node.name);
                continue;
            }
        };
        // probe() 会真读一次：枚举里挂着不代表此刻连着（系统缓存会留住已关机的设备），
        // 读不到就当它不在，别给一台关机的手柄建声明
        if !dev.probe() {
            continue;
        }
        seen.push((node.vid, node.pid));
        // 名字用系统在蓝牙里认到的那个：用户看到的就是这个名字
        let name = if node.name.is_empty() {
            format!("蓝牙设备 {:04X}:{:04X}", node.vid, node.pid)
        } else {
            node.name.clone()
        };
        info!(
            "[discover] 蓝牙：{}（{:04X}:{:04X}）走标准电池服务，自动加入",
            name, node.vid, node.pid
        );
        out.push(Profile {
            name: name.clone(),
            short_name: None,
            protocol: Protocol::Ble,
            transport: Transport::Direct,
            vid: node.vid,
            pid: node.pid,
            usage_page: None,
            usage: None,
            battery_feature: None,
            battery_kind: BatteryKind::Percent,
            raw: None,
            slots: vec![1],
            name_feature: None,
            identify_contains: None,
            protocol_source: "蓝牙 SIG 公开标准：Battery Service 0x180F / Battery Level 0x2A19"
                .into(),
            verification: "本机自动发现：设备自报百分比，未与官方软件对照".into(),
        });
    }

    // ---- HID++：只对罗技设备发这类报文 ----
    for info in api.device_list() {
        let (vid, pid) = (info.vendor_id(), info.product_id());
        if !HIDPP_VENDORS.contains(&vid) {
            continue;
        }
        if known.contains(&(vid, pid)) || seen.contains(&(vid, pid)) {
            continue;
        }
        // 同一个 USB 设备往往暴露好几个接口，全试一遍没有意义，
        // 认出一个就够了（剩下的接口读出来也是同一台设备的同一份电量）
        let Ok(dev) = info.open_device(api) else {
            continue;
        };
        let conn = Hidpp::new(dev);

        // 先分清它是接收器还是直连设备，再决定怎么问 —— 顺序反了会在接收器上
        // 只认住 index 1，设备漂到别的 slot 就再也读不到
        let fallback = hid_name(info);
        let found = if looks_direct(&conn) {
            try_direct(&conn)
                .map(|(feat, kind)| (DIRECT_IDX, feat, kind, Transport::Direct, vec![DIRECT_IDX]))
        } else {
            None
        };
        let (idx, feat, kind, transport, slots) = match found {
            Some(v) => v,
            None => match try_receiver(&conn) {
                // 接收器上可能有好几台设备、slot 还会漂移，所以把 slot 全写进声明：
                // 空 slot 不会应答，写了不会多出一台设备，反而能接住漂移
                Some((slot, feat, kind)) => (
                    slot,
                    feat,
                    kind,
                    Transport::Receiver,
                    RECEIVER_SLOTS.collect(),
                ),
                None => continue,
            },
        };
        let name = best_name(&conn, idx, fallback);
        info!(
            "[discover] HID++：{}（{vid:04X}:{pid:04X}）电量 feature {feat:#06X}，按{}模式自动加入",
            name,
            if transport == Transport::Receiver {
                "接收器"
            } else {
                "直连"
            }
        );
        seen.push((vid, pid));
        out.push(hidpp_profile(name, vid, pid, feat, kind, transport, slots));
    }
    out
}

fn hid_name(info: &hidapi::DeviceInfo) -> String {
    let s = info.product_string().unwrap_or("").trim();
    if s.is_empty() {
        format!(
            "USB 设备 {:04X}:{:04X}",
            info.vendor_id(),
            info.product_id()
        )
    } else {
        s.to_string()
    }
}

fn hidpp_profile(
    name: String,
    vid: u16,
    pid: u16,
    feature: u16,
    kind: BatteryKind,
    transport: Transport,
    slots: Vec<u8>,
) -> Profile {
    Profile {
        name,
        short_name: None,
        protocol: Protocol::Hidpp20,
        transport,
        vid,
        pid,
        usage_page: None,
        usage: None,
        battery_feature: Some(feature),
        battery_kind: kind,
        raw: None,
        slots,
        // 让它每轮都去问设备自己的名字：面板里显示的名字才是用户认得的那台，
        // 而不是「第一次发现时恰好挂在 slot 1 的那台」的名字
        name_feature: Some(F_DEVICE_NAME),
        identify_contains: None,
        protocol_source: format!(
            "HID++ 2.0 公开 feature {feature:#06X}（本机自动探测，设备自报支持）"
        ),
        verification: match kind {
            BatteryKind::Voltage => {
                "本机自动发现：读到的是电压，按共用放电曲线换算，未与官方软件对照".into()
            }
            BatteryKind::Percent => "本机自动发现：设备自报百分比，未与官方软件对照".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hidpp::Battery;

    #[test]
    fn 电压式只认像电压的数() {
        let good = Battery {
            percent: 88,
            mv: 4050,
            charging: false,
        };
        let zero = Battery {
            percent: 0,
            mv: 0,
            charging: false,
        };
        assert!(plausible(&good, BatteryKind::Voltage));
        assert!(!plausible(&zero, BatteryKind::Voltage));
        // 边界：锂电放空与满电的两端都要算合理
        let low = Battery {
            percent: 1,
            mv: 2500,
            charging: false,
        };
        let high = Battery {
            percent: 100,
            mv: 5000,
            charging: false,
        };
        assert!(plausible(&low, BatteryKind::Voltage));
        assert!(plausible(&high, BatteryKind::Voltage));
    }

    #[test]
    fn 对非罗技厂商不动手() {
        // 这条守的是「别对着别人的设备发私有报文」：
        // HID++ 是罗技的协议，厂商清单里没有的 VID 一律跳过
        assert!(HIDPP_VENDORS.contains(&0x046D));
        assert!(!HIDPP_VENDORS.contains(&0x0951));
    }
}
