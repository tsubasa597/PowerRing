//! 蓝牙电量探测（临时工具，验证完可能删掉）
//!
//! 目的：确认「Windows 上能不能用标准 BLE Battery Service 读出百分比」。
//! UUID 都是蓝牙 SIG 定的公开标准，不涉及任何厂商私有协议。
//!
//!   cargo run --release --example ble_probe

use windows::core::{Result, GUID};
use windows::Devices::Bluetooth::BluetoothCacheMode;
use windows::Devices::Bluetooth::GenericAttributeProfile::{
    GattCommunicationStatus, GattDeviceService, GattServiceUuids,
};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Storage::Streams::DataReader;

/// 标准 Battery Level characteristic：一个字节，0..100
const BATTERY_LEVEL: GUID = GUID::from_u128(0x00002A19_0000_1000_8000_00805F9B34FB);

fn main() -> Result<()> {
    let selector = GattDeviceService::GetDeviceSelectorFromUuid(GattServiceUuids::Battery()?)?;
    println!("AQS = {selector}");

    let devices = DeviceInformation::FindAllAsyncAqsFilter(&selector)?.get()?;
    println!("找到 {} 个设备\n", devices.Size()?);

    for d in devices {
        let name = d.Name()?;
        let id = d.Id()?;
        println!("== {name}");
        println!("   id = {id}");

        // 关键问题：设备关机后还能枚举到、缓存里还有旧值。
        // 看看蓝牙设备的连接状态能不能把「开机在线」和「只留了个残影」区分开
        println!("   连接状态 = {}", conn(&id));

        let svc = GattDeviceService::FromIdAsync(&id)?.get()?;
        let chars = svc.GetCharacteristicsAsync()?.get()?.Characteristics()?;
        for ch in chars {
            if ch.Uuid()? != BATTERY_LEVEL {
                continue;
            }
            // 两种模式都读：ReadValueAsync 走缓存，ReadValueWithCacheModeAsync
            // 指定 Uncached 会真的发一次 GATT 读请求到设备上。两边对比就能看出
            // 系统有没有给我们一个过期的值
            let cached = unpack(&ch, BluetoothCacheMode::Cached);
            let fresh = unpack(&ch, BluetoothCacheMode::Uncached);
            let show = |r: &std::result::Result<u8, String>| match r {
                Ok(v) => format!("{v}"),
                Err(e) => format!("失败（{e}）"),
            };
            println!("   缓存读 = {}", show(&cached));
            println!("   实时读 = {}", show(&fresh));
        }
        println!();
    }
    Ok(())
}

/// 用 BluetoothLEDevice 的连接状态判断设备是不是真的还在连着
fn conn(id: &windows::core::HSTRING) -> String {
    let Ok(op) = windows::Devices::Bluetooth::BluetoothLEDevice::FromIdAsync(id) else {
        return "FromIdAsync 发起失败".to_string();
    };
    let Ok(dev) = op.get() else {
        return "FromIdAsync 失败（设备已不在？）".to_string();
    };
    match dev.ConnectionStatus() {
        Ok(s) => format!("{s:?}"),
        Err(e) => format!("取状态失败 {e}"),
    }
}

/// 带诊断信息的 Uncached 读取：失败时把 WinRT 给的错误码和协议错误一并吐出来
fn unpack(
    ch: &windows::Devices::Bluetooth::GenericAttributeProfile::GattCharacteristic,
    mode: BluetoothCacheMode,
) -> std::result::Result<u8, String> {
    let op = ch
        .ReadValueWithCacheModeAsync(mode)
        .map_err(|e| format!("发起读取失败 {e}"))?;
    let r = op.get().map_err(|e| format!("异步等待失败 {e}"))?;
    let st = r.Status().map_err(|e| format!("取状态失败 {e}"))?;
    if st != GattCommunicationStatus::Success {
        // ProtocolError 是 BLE 层的错误码，Uncached 时才可能有值
        let pe = match r.ProtocolError() {
            Ok(e) => format!("{e:?}"),
            Err(_) => "无".to_string(),
        };
        return Err(format!("status={st:?} protocolError={pe}"));
    }
    let buf = r.Value().map_err(|e| format!("取值失败 {e}"))?;
    let len = buf.Length().map_err(|e| format!("取长度失败 {e}"))? as usize;
    let mut out = vec![0u8; len];
    DataReader::FromBuffer(&buf)
        .map_err(|e| format!("建 Reader 失败 {e}"))?
        .ReadBytes(&mut out)
        .map_err(|e| format!("读字节失败 {e}"))?;
    out.first()
        .copied()
        .ok_or_else(|| "读到 0 字节".to_string())
}
