# 安全说明

## 报告问题

请走 GitHub 的[私下漏洞报告](https://github.com/vkeyzhen/PowerRing/security/advisories/new)，**不要开公开 Issue**。

七天之内会收到一次回复，哪怕只是「收到了，正在看」。

## 支持范围

只维护最新一次 Release。旧版本不单独回补安全问题 —— 单文件 exe，换掉就是升级，没有迁移成本。

## 这个程序会碰什么

摆在这里，方便你判断风险能不能接受：

| 能力 | 为什么需要 | 边界 |
| --- | --- | --- |
| 打开 HID 设备 | 电量只能从设备自己的接口读 | 只打开匹配 `devices/*.yaml` 里声明的 VID/PID/usage 的那些接口；不发写命令，不改设备状态 |
| 读标准蓝牙电池服务 | 蓝牙设备走 `0x180F` / `0x2A19` | 只读这一个特征，不配对、不绑定、不碰其它 GATT 服务 |
| 读写 `%APPDATA%\PowerRing\` | 存配置、写日志 | 只有 `config.json` 与 `log.txt` 两个文件 |
| 写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` | 开机自启 | 只在设置里显式打开时写 |
| 读同一个注册表键 | 判断自启项是不是自己（exe 换过路径要认作「没开」） | 只读这一个值 |

**不碰**：网络（没有任何 socket、更新检查或遥测）、剪贴板、键盘与鼠标输入、其它进程的内存、提权（全程不需要管理员权限）。

讲清楚一件事：这个项目里 Win32 调用全是 `unsafe`，那是 FFI 的必需，不代表「有不安全代码在跑业务逻辑」。安全的实际边界是上面这张表 —— 它做的事就这些。

## 两类需要留心的输入

1. **`devices/*.yaml`**：程序启动时读这个目录，YAML 决定「往哪个接口发哪些字节」。字节长度、下标都做了越界检查，但**别把不可信来源的 YAML 直接丢进 `devices/`** —— 拿到写权限的人等于拿到了配置权。
2. **`log.txt`**：记录设备名、原始读数、失败次数。没有个人信息，往 Issue 里贴之前扫一眼就行。

## 供应链

依赖很少（`windows`、`hidapi`、`tray-icon`、`serde` 几项），每次提交在 GitHub Actions 上跑 `cargo fmt --check`、`clippy -D warnings`、`cargo test`。Release 由 tag 触发、在 CI 里从源码构建，产物可对照源码复核。
