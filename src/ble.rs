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
//!
//! ## 为什么读缓存，而不是实时
//!
//! WinRT 有两种读法：`ReadValueAsync()` 走缓存，
//! `ReadValueWithCacheModeAsync(Uncached)` 会真的往设备发一次 GATT 读请求。
//! 直觉上后者更准，实测却不是这么回事 —— 本机两台设备上 Uncached 一律返回
//! `Unreachable`，包括那台肯定在线的设备。Windows 不替桌面进程维持这些设备的
//! GATT 会话，硬读只会读不到。
//!
//! 于是读缓存，而这么选还有个额外的好处：**我们读到的值与 Windows 蓝牙设置里
//! 显示的是同一个**（2026-10-23 交叉对照，两边同为 12%），用户不会看到两个软件
//! 各说各话。代价是刷新节奏由系统掌握，实测几分钟一次。
//!
//! 副作用要清楚：系统会长期留着已配对设备的最后读数，哪怕那台设备已经关机、
//! 甚至早就不在身边。本机就有一台放在公司的键盘，长年能枚举到、能读出 100%。
//! 这是 Windows 的行为，本项目选择跟随，并在 `devices/xbox_controller.yaml` 的
//! notes 里写明了。

use windows::core::GUID;
use windows::Devices::Bluetooth::{
    BluetoothConnectionStatus, BluetoothLEDevice, GenericAttributeProfile as GP,
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
        GP::GattDeviceService::GetDeviceSelectorFromUuid(GP::GattServiceUuids::Battery().ok()?)
            .ok()?;
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
    /// 设备接口路径。查连接状态时要用，所以留在手上
    id: String,
    ch: GP::GattCharacteristic,
}

impl Dev {
    /// 打开设备的电池服务并找到 Battery Level 通道。
    /// 打开了服务但里面没有那个通道也算失败 —— 有些厂商挂了 0x180F 却不放 0x2A19
    pub fn open(id: &str) -> Result<Dev, String> {
        let svc = GP::GattDeviceService::FromIdAsync(&windows::core::HSTRING::from(id))
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
                    return Ok(Dev {
                        id: id.to_string(),
                        ch,
                    });
                }
            }
        }
        Err("设备挂了 Battery Service 但里面没有 Battery Level 通道".to_string())
    }

    /// 读一次电量。标准只给百分比，**不含是否在充电的信息** —— 所以 charging 恒为 false
    ///
    /// 设备不在就不给值。这不是多此一举：缓存会把关机设备的最后读数一直留着，
    /// 光看「读没读到」根本分不出它在不在（详见模块头注释），所以这里额外看一眼
    /// 连接状态，交给上层用「连续几次读不到」的既有机制把它请出列表。
    pub fn battery(&self) -> Option<Battery> {
        if !self.connected() {
            return None;
        }
        let percent = self.raw()?;
        Some(Battery {
            percent,
            mv: 0,
            charging: false,
        })
    }

    /// 扫描时用：既要读得到值，也要设备在线。两条挨在一起讲清楚它们的分工 ——
    /// `raw()` 只能证明「缓存里有个数」，`connected()` 才能证明「这台设备还在这儿」
    pub fn probe(&self) -> bool {
        self.connected() && self.raw().is_some()
    }

    /// 设备的蓝牙连接是不是还连着。
    ///
    /// ⚠️ **这里的验证是不对称的，改之前先看清楚**：
    ///   离线的一半验证过了 —— 关机充电的手柄、放在公司的键盘，都是 Disconnected，
    ///   判对了。在线的一半还没验 —— 因为手上暂时没有在线的蓝牙设备可对照。
    /// 如果哪天发现「正在用的设备莫名消失」，第一个嫌疑就是这个函数返回了 false，
    ///   先看这里的日志，别急着怀疑上面的轮询逻辑。
    ///
    /// 取不到状态时按在线算（返回 true）：宁可多显示一台，也别因为一次 API
    /// 抽风把正在用的设备从托盘里摘掉。真正离线的设备会**连续**失败，上层
    /// 有三次容错，一次误判不会被放大成"设备消失"。
    fn connected(&self) -> bool {
        let Ok(op) = BluetoothLEDevice::FromIdAsync(&windows::core::HSTRING::from(&self.id)) else {
            warn!("[ble] 建 BluetoothLEDevice 失败，按在线处理");
            return true;
        };
        match op.get() {
            Ok(dev) => match dev.ConnectionStatus() {
                // 不记 normal 情况：轮询每几秒一次，正常情况的日志会把滚动日志冲干净，
                // 而「设备离线」这件事上层本来就有「读取失败」的日志在讲
                Ok(s) => s == BluetoothConnectionStatus::Connected,
                Err(e) => {
                    warn!("[ble] 取连接状态失败 {e}，按在线处理");
                    true
                }
            },
            // FromIdAsync 本身失败多半是设备已不在，但也有概率是系统忙。
            // 这里保守按在线处理，交给上层的连续失败计数兜底
            Err(e) => {
                warn!("[ble] FromIdAsync 失败 {e}，按在线处理");
                true
            }
        }
    }

    fn raw(&self) -> Option<u8> {
        // 有意不用 Uncached：实测在本机两台设备上都返回 Unreachable，
        // 见模块头注释。这里读到的就是 Windows 自己在用的那个值
        let r = self.ch.ReadValueAsync().ok()?.get().ok()?;
        if r.Status().ok()? != GP::GattCommunicationStatus::Success {
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
