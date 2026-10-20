# PowerRing

Windows 无线外设电量监测工具。托盘里一圈电量环，一眼看清鼠标、耳机、手柄还剩多少电——不用为每个品牌装一个厂商软件。

Rust 编写，常驻内存 20 MB 量级，无管理员权限可运行，无云端、无账号、无上报。

## 当前状态

| 里程碑 | 状态 | 说明 |
| --- | --- | --- |
| M0 技术验证 | ✅ 通过 | Python + hidapi 读出 GPW 电量，与 G HUB 误差 1% |
| M0.5 风险验证 | ✅ 通过 | 托盘 + hidapi + Windows 消息泵 + 配置解析跑通，产物 721 KB |
| M1 MVP | 🚧 进行中 | 已实现：YAML 声明驱动的多设备扫描、电量环图标、滞回平滑 |

## 用法

```bash
cargo run --release
```

托盘区出现一圈电量环，缺口大小对应剩余电量；悬停显示设备名与百分比；右键可退出。
绿 → 黄 → 红 表示电量由高到低，充电时转为蓝色；读不到时显示灰色环与 `--`（**绝不用 0% 表示"读不到"**）。

## 加一台设备

复制 `devices/logitech_gpw.yaml` 改几行即可，不需要改 Rust 代码：

```yaml
name: Logitech G PRO Wireless
protocol: hidpp20          # 协议族：hidpp20 / hidpp10 / hyperx / ...
transport: hid
match:                     # 接口匹配条件
  vid: "0x046D"
  pid: "0xC539"
slots: [1, 2, 3, 4, 5, 6]  # 接收器上要探测的槽位
battery:
  feature: "0x1001"        # 电量 feature
  kind: voltage            # voltage = 读 mV 换算；percent = 直接百分比
identify:
  name_feature: "0x0005"
  contains: "PRO WIRELESS"
protocol_source: Solaar（GPL-3.0），仅移植协议常量，未复制源码
verification: verified
```

注意：换**协议族**（例如 HyperX 天箭走另一套私有协议）仍需写 Rust 代码，填表只对同一协议族内新增设备生效。

## 构建

需要 Rust 工具链（Windows 默认 `x86_64-pc-windows-msvc`，依赖 VS 2022 生成工具）。

```bash
cargo build --release
```

产物：`target/release/powerring.exe`

## 已验证的协议事实

**罗技 G PRO Wireless**（接收器 `0x046D:0xC539`，HID++ 2.0）

- 不支持 `0x1004` 统一电量，只有 `0x1001` 电压式（mV → 百分比走放电曲线）
- 长报文必须写满 **20 字节**，短报文固定 **7 字节**，否则设备不响应或回 `0x8F`
- 判断接收器 slot 是否有设备要用 `getFeature`，**不能用 ping**
- 实测：3744 mV → 28%，G HUB 同时间 29%，误差 1%

**HyperX Cloud Flight**（`0x0951:0x1723`，usage `0xFF90`/`0x0303`）：已枚举到，尚未接线

## 许可证红线

协议常量与报文格式属于事实信息，可以移植；**禁止直接复制** Solaar / HeadsetControl / OpenRazer 等 GPL 项目的源码。每份设备配置都必须填写 `protocol_source` 注明出处。

本项目自身采用 **MIT**。
