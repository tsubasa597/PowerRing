//! 蓝牙电量探测（临时工具，验证完可能删掉）
//!
//! 目的：确认「Windows 上能不能用标准 BLE Battery Service 读出百分比」。
//! UUID 都是蓝牙 SIG 定的公开标准，不涉及任何厂商私有协议。
//!
//!   cargo run --release --example ble_probe

use windows::core::{Result, GUID};
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

        let svc = GattDeviceService::FromIdAsync(&id)?.get()?;
        let chars = svc.GetCharacteristicsAsync()?.get()?.Characteristics()?;
        for ch in chars {
            if ch.Uuid()? != BATTERY_LEVEL {
                continue;
            }
            let r = ch.ReadValueAsync()?.get()?;
            if r.Status()? != GattCommunicationStatus::Success {
                println!("   读取失败（status != Success）");
                continue;
            }
            let buf = r.Value()?;
            let len = buf.Length()? as usize;
            let rd = DataReader::FromBuffer(&buf)?;
            let mut bytes = vec![0u8; len];
            rd.ReadBytes(&mut bytes)?;
            println!("   原始字节 = {bytes:?}  ->  {}%", bytes[0]);
        }
        println!();
    }
    Ok(())
}
