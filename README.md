# PowerRing

> English: [README.en.md](README.en.md)

Windows 无线外设电量监测。托盘里一圈电量环，一眼看清鼠标、耳机还剩多少电 —— 不用为每个品牌装一个厂商软件。

- **单文件 exe，约 0.9 MB**，免安装、不需要管理员权限
- **不联网、无账号、无遥测**，所有数据留在本机
- **加一台设备 = 加一份 YAML**，不需要改代码（详见 [加一台设备](#加一台设备)）

## 现在能做什么

| 功能 | 说明 |
| --- | --- |
| 托盘电量环 | 缺口大小对应剩余电量。绿 → 黄 → 红表示由高到低，充电时转蓝；**读不到时显示灰环，绝不用 0% 表示「读不到」** |
| 悬停看数字 | 鼠标停在图标上显示各设备百分比；多台时用短名，按电量从低到高排 |
| 左键点击 | 开/关电量面板，列出每台设备的完整名字与电量 |
| 右键菜单 | 显示电量面板 / 立即刷新 / 设置… / 退出 |
| 低电量提醒 | 跨过阈值才弹一次，每档只弹一次，卡片锚在托盘旁边，不用去找它在哪 |
| 开机自启 | 写 `HKCU\...\Run`，不需要提权；同一个互斥量保证不会冒出第二个托盘图标 |
| 界面配色 | 跟随系统 / 浅色 / 深色三态。任务栏是深色而应用是浅色时，可以单独强制深色 |
| 自动发现 | 每 60 秒重扫一遍总线：刚开机的设备会自己进来，关机的不会挂着旧电量骗人 |
| 日志 | `%APPDATA%\PowerRing\log.txt`，超过 1 MB 滚动 |

设置窗口里可以调刷新间隔、警告阈值与严重阈值、提醒开关（带「试一次」按钮，改完当场验证）、开机自启、界面配色。

## 已验证的设备

| 设备 | 接口 | 电量来源 | 状态 |
| --- | --- | --- | --- |
| Logitech G PRO Wireless | 接收器 `046D:C539`，usage page `FF00` / usage `0002`，slot 1–6 | HID++ 2.0 `0x1001` 电压式（mV → 放电曲线） | 已验证。3744 mV 读 28%，同期 G HUB 显示 29% |
| HyperX Cloud Flight（天箭） | 接收器 `0951:1723`，usage page `FF90` / usage `0303` | 私有报文 `21 FF 05`（接收器直连模式） | 已验证。数值 231 读 60% |

设备声明在 `devices/` 下，每份 YAML 都写了 `protocol_source`（协议常量出处）与 `verification`（是否已上真机验证）。**没上过机的一律标 `unverified`，不会被当作可信数据。**

## 用法

下载 Release 里的 zip，解压到任意目录（比如 `C:\Tools\PowerRing`），双击 `powerring.exe` 即可。目录结构必须是 exe 与 `devices` 平级：

```
PowerRing/
├─ powerring.exe
└─ devices/
   ├─ logitech_gpw.yaml
   └─ hyperx_cloud_flight.yaml
```

> Windows 10 会把新托盘图标折叠进通知区域的 `^` 里。如果没看到，去「任务栏设置 → 通知区域 → 选择哪些图标显示在任务栏上」把它打开。

## 加一台设备

复制 `devices/` 下最接近的一份 YAML 改几行即可，**不需要改 Rust 代码**。两种写法：

**HID++ 2.0 设备**（罗技系）走「feature 号 + 取值方式」：

```yaml
name: "Logitech G PRO Wireless"
short: "GPW"              # 托盘悬停用；不填就用全名
protocol: "hid++2.0"
transport: "receiver"     # 经接收器按 slot 寻址；设备直连用 "direct"

match:                    # 匹配哪个 HID 接口
  vendor_id: "0x046D"
  product_id: "0xC539"
  usage_page: "0xFF00"
  usage: "0x0002"

slots: [1, 2, 3, 4, 5, 6] # 接收器上要探测哪些槽位，省略默认 1..6
battery:
  feature: "0x1001"       # 电量 feature
  kind: "voltage"         # voltage = 读 mV 换算；percent = 读出来就是百分比
identify:                 # 同一个接收器上可能挂着别的设备，用它做二次确认
  name_feature: "0x0005"
  contains: "PRO WIRELESS"
protocol_source: "Logitech HID++ 2.0，常量参考 Solaar（GPL-3.0）公开文档"
verification: "verified"
```

**非 HID++ 设备**走 `raw`：把「发什么报文、读哪几个字节、怎么换算」全部写进声明。下面是 HyperX 天箭的真实配置，它没有一行 Rust 代码被改动就接进来了：

```yaml
protocol: "raw"
transport: "direct"
match:
  vendor_id: "0x0951"
  product_id: "0x1723"
  usage_page: "0xFF90"
  usage: "0x0303"
battery:
  request: "21 FF 05"     # 请求报文，首字节是 report id
  reply_len: 20           # 很多设备对长度敏感，短了干脆不回
  state_index: 3          # 响应里「档位」字节的下标
  value_index: 4          # 响应里「数值」字节的下标
  charging_states: ["0x10", "0x11"]
  table:                  # 分段换算表：数值落在哪一段取哪个百分比
    "0x0e":
      - [89, 10]
      - [239, 60]
      - [255, 65]
    "0x0f":
      - [19, 70]
      - [129, 95]
      - [255, 100]
```

用分段表而不是公式：这些设备的原始数值与百分比**不是线性关系**，线性拟合在高电量段能差出十几个百分点。

完整字段说明与手把手抓包流程见 [docs/adding-a-device.md](docs/adding-a-device.md)。想把自己的设备贡献进来，直接用仓库里的 Issue 模板提。

## 构建

需要 Rust 工具链（Windows 默认 `x86_64-pc-windows-msvc`，依赖 VS 2022 生成工具）。

```bash
cargo build --release
```

产物：`target/release/powerring.exe`（约 0.9 MB）。

## 已验证的协议事实

**罗技 G PRO Wireless**（HID++ 2.0）

- 不支持 `0x1004` 统一电量，只有 `0x1001` 电压式
- 长报文必须写满 **20 字节**，短报文固定 **7 字节**，长度不对设备不响应或回 `0x8F`
- 判断接收器 slot 有没有设备要用 `getFeature`，**不能用 ping**
- 电压读数有约 1 mV 抖动 —— 代码侧做了滞回平滑，变化不足 2 个百分点不更新
- HID 报文里的设备名是定长加 `0x00` 填充的，`trim()` 不会去掉 NUL

**HyperX Cloud Flight**

- 接收器暴露 4 个接口，**只有 `FF90:0303` 会回应电量请求**，其余三个（`000C:0001` / `FF00:0001` / `FFC0:0001`）发了没反应 —— 探测时别只试一个
- 耳机没开机时探测无响应，程序会正确地把它排除掉，而不是显示 0%
- 请求报文的 report id 是 `0x21`，比常见的 `0x00`–`0x10` 都大，逐个试的时候容易漏

## 已知限制

- 蓝牙设备（如 AirPods）暂不支持：它们的电量走 BLE GATT，且通常由系统自己展示
- Xbox 手柄待支持
- 读不到电量时显示灰环，不会猜一个数

## 许可证红线

协议常量与报文格式属于事实信息，可以移植；**禁止直接复制** Solaar / HeadsetControl / OpenRazer 等 GPL 项目的源码。每份设备声明都必须填写 `protocol_source` 注明出处；引用 GPL 项目时只允许把常量当事实抄进来，不允许引用其代码。

本项目自身采用 **MIT**，见 [LICENSE](LICENSE)。
