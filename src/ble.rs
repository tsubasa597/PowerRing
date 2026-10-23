//! 蓝牙 BLE 电量：**标准** Battery Service
//!
//! 蓝牙 SIG 定了两个公开 UUID：
//!   服务 0x180F（Battery Service）
//!   通道 0x2A19（Battery Level）—— 读出来一个字节，就是 0..100 的百分比
//!
//! 这条路的特殊之处在于**完全不需要逆向**：不像罗技的 HID++、HyperX 的私有报文
//! 得挨家猜，只要是符合标准的蓝牙设备，系统已经把通道开好了。
//! 代价是只能在走 BLE 协议的设备上用 —— 大部分无线键鼠用的是私有 2.4G，不在此列。
//!
//! WinRT 异步一律用 `get()` 同步等结果：轮询线程每几分钟才调一次，
//! 为它引入一套异步运行时不值得，阻塞等待反而让出错路径简单得多。

use windows::core::GUID;
use windows::Devices::Bluetooth::GenericAttributeProfile::{
    GattCharacteristic, GattCommunicationStatus, GattDeviceService, GattServiceUuids,
};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Storage::Streams::DataReader;

use crate::hidpp::Battery;
use crate::{info, warn};

/// 0x2A19 Battery Level
const BATTERY_LEVEL: GUID = GUID::from_u128(0x00002A19_0000_1000_8000_00805F9B34FB);

/// 枚举出的一个「带电池服务的蓝牙设备」
pub struct Node {
    /// 设备接口路径，打开服务时要用
    pub id: String,
    /// 系统显示名
    pub name: String,
    pub vid: u16,
    pub pid: u16,
}

/// 列出本机所有暴露 Battery Service 的 BLE 设备。
///
/// 失败返回 None 而不 panic：蓝牙无线电被关掉、系统没有蓝牙时都会失败，
/// 这只是「这类设备暂时不可用」，不该把整个程序拖下水。
pub fn enumerate() -> Option<Vec<Node>> {
    let selector =
        GattDeviceService::GetDeviceSelectorFromUuid(GattServiceUuids::Battery().ok()?).ok()?;
    let devs = DeviceInformation::FindAllAsyncAqsFilter(&selector)
        .ok()?
        .get()
        .ok()?;
    let mut out = Vec::new();
    for d in devs {
        let Ok(id) = d.Id() else { continue };
        let id = id.to_string();
        let name = d.Name().map(|n| n.to_string()).unwrap_or_default();
        let Some((vid, pid)) = ids_from(&id) else {
            // 解析不出厂商号的设备没法跟任何声明对上，只能跳过
            warn!("[ble] 设备 {name} 的路径里没有可用的 VID/PID，跳过");
            continue;
        };
        info!("[ble] 发现 {name} (VID {vid:#06X} PID {pid:#06X})");
        out.push(Node { id, name, vid, pid });
    }
    Some(out)
}

/// 从设备接口路径里抠出厂商号和产品号。
///
/// 路径形如：
///   \\?\BTHLEDevice#{0000180f-...}_Dev_VID&02045e_PID&0b13_REV&0517_3cfa0673b8b8#...
///
/// 注意 VID 字段是 **6 位**十六进制（前 4 位前缀 + 真正的 VID），Xbox 的 0x045E
/// 在这里写成 02045E —— 所以只取**最后 4 位**，照抄整个字段会得到一个不存在的厂商号
fn ids_from(id: &str) -> Option<(u16, u16)> {
    let lower = id.to_ascii_lowercase();
    let vid = field(&lower, "vid&")?;
    let pid = field(&lower, "pid&")?;
    Some((last4_hex(&vid)?, hex16(&pid)?))
}

/// 取出 `key` 后面那段连续字符（到下划线为止），如 "vid&02045e"
fn field(id: &str, key: &str) -> Option<String> {
    let rest = id.split(key).nth(1)?;
    Some(rest.split('_').next()?.to_string())
}

/// "02045e" -> 0x045E（取末尾 4 位）
fn last4_hex(s: &str) -> Option<u16> {
    let tail = s.get(s.len().checked_sub(4)?..)?;
    u16::from_str_radix(tail, 16).ok()
}

fn hex16(s: &str) -> Option<u16> {
    u16::from_str_radix(s, 16).ok()
}

/// 一个已经打开的电池通道。持有它就不用每轮重新发现一次 GATT
pub struct Dev {
    ch: GattCharacteristic,
}

impl Dev {
    /// 打开设备的电池服务并找到 Battery Level 通道。
    /// 打开了服务但里面没有那个通道也算失败 —— 有些厂商挂了 0x180F 却不放 0x2A19
    pub fn open(id: &str) -> Result<Dev, String> {
        let svc = GattDeviceService::FromIdAsync(&windows::core::HSTRING::from(id))
            .map_err(|e| format!("打开 GATT 服务失败：{e}"))?
            .get()
            .map_err(|e| format!("等待 GATT 服务超时：{e}"))?;
        let chars = svc
            .GetCharacteristicsAsync()
            .map_err(|e| format!("列通道失败：{e}"))?
            .get()
            .map_err(|e| format!("列通道超时：{e}"))?
            .Characteristics()
            .map_err(|e| format!("取通道列表失败：{e}"))?;
        for ch in chars {
            if let Ok(u) = ch.Uuid() {
                if u == BATTERY_LEVEL {
                    return Ok(Dev { ch });
                }
            }
        }
        Err("设备挂了 Battery Service 但里面没有 Battery Level 通道".to_string())
    }

    /// 读一次电量。标准只给百分比，**不含是否在充电的信息** —— 所以 charging 恒为 false
    pub fn battery(&self) -> Option<Battery> {
        let percent = self.raw()?;
        Some(Battery {
            percent,
            mv: 0,
            charging: false,
        })
    }

    /// 扫描时用：读得到就算在线。读不到不一定是关机，也可能是暂时连不上，
    /// 所以由上层用连续失败次数来判断要不要移出列表
    pub fn probe(&self) -> bool {
        self.raw().is_some()
    }

    fn raw(&self) -> Option<u8> {
        let r = self.ch.ReadValueAsync().ok()?.get().ok()?;
        if r.Status().ok()? != GattCommunicationStatus::Success {
            return None;
        }
        let buf = r.Value().ok()?;
        let len = buf.Length().ok()? as usize;
        if len == 0 {
            return None;
        }
        let rd = DataReader::FromBuffer(&buf).ok()?;
        // 标准规定至少 1 字节，但也有设备多给几位，这里只读第一个
        rd.ReadByte().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 真机的设备路径（Xbox 手柄，键名大小写是系统给的）
    const XBOX: &str = r"\\?\BTHLEDevice#{0000180f-0000-1000-8000-00805f9b34fb}_Dev_VID&02045e_PID&0b13_REV&0517_3cfa0673b8b8#8&19a62b31&1&0012#{6e3bb679-4372-40c8-9eaa-4509df260cd8}";

    #[test]
    fn vid字段取末尾四位才是真厂商号() {
        assert_eq!(ids_from(XBOX), Some((0x045E, 0x0B13)));
    }

    #[test]
    fn pid字段直接就是产品号() {
        assert_eq!(ids_from(XBOX).map(|(_, pid)| pid), Some(0x0B13));
    }

    #[test]
    fn 路径里没有厂商号就返回空() {
        assert_eq!(ids_from(r"\\?\BTHLEDevice#{0000180f}_Dev_x"), None);
    }

    #[test]
    fn 另一位设备也能解析() {
        // MK75 键盘：VID 字段写成 023554，真正厂商号是 0x3554
        let id = r"\\?\BTHLEDevice#{0000180f-0000-1000-8000-00805f9b34fb}_Dev_VID&023554_PID&fa07_REV&6701_c99d93fe81ff#8&1f08d058&0&000f#{6e3bb679}";
        assert_eq!(ids_from(id), Some((0x3554, 0xFA07)));
    }
}
