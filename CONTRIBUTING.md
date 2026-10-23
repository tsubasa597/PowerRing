# 贡献指南

欢迎。这个项目最有价值的贡献是**新设备声明** —— 一份 YAML 就能让一台原本没人管的设备被读出来，而且不需要会写代码。

- 想让自己的设备被支持：先读 [docs/adding-a-device.md](docs/adding-a-device.md)
- 只想报个数据（某个原始数值对应百分之几）也行：开个 Issue 贴上去，标定可以交给别人继续
- 想改代码：往下看

## 几条硬规矩

这几条是项目能保持不臃肿的原因，提交前请对照一下：

1. **设备常量不许进代码。** VID/PID、feature 号、报文字节、换算表一律住在 `devices/*.yaml` 里。`src/` 下出现任何厂商魔数都属于需要解释的例外。
2. **不联网。** 没有遥测、没有更新检查、没有崩溃上报。这个仓库不会被改成依赖服务端的样子。
3. **读不到 ≠ 0%。** 读数失败必须显示为未知状态（灰环），不许猜数、不许沿用上一次的值骗人。同理，设备关机后必须从列表里消失。
4. **配色集中在 `src/theme.rs`。** 绘制代码里不许写死颜色，否则深色模式会漏这一块。
5. **日志走 `info!` / `warn!`。** 程序是 `windows` 子系统，没有控制台，别用 `println!` —— 写了也看不到。日志落 `%APPDATA%\PowerRing\log.txt`。
6. **只在特定条件下触发的功能要有手动入口。** 低电量提醒只在跨阈值时弹，所以设置窗口里有「试一次」按钮。没有入口的话，用户和我都没法验证它到底有没有生效。
7. **协议常量可以移植，源码不可以复制。** 引用 GPL 项目时请务必在 `protocol_source` 里注明出处，并且只取事实，不取代码。

## 开发流程

```bash
git clone https://github.com/vkeyzhen/PowerRing.git
cd PowerRing

cargo fmt              # 提交前必须
cargo clippy -- -D warnings
cargo test
cargo build --release
```

环境：Windows + Rust stable（`x86_64-pc-windows-msvc`），依赖 VS 2022 生成工具。目前只在 Windows 上构建，跨平台支持暂不在计划内。

调试时可以设环境变量 `POWERRING_OPEN_SETTINGS=1`，启动时直接打开设置窗口，省一次右键。

> Windows 10 会把新托盘图标折叠进通知区域的 `^` 里。找不到图标先去任务栏设置里打开显示。

## 提交

commit message 用中文，前缀保持这套：

- `feat:` 新功能
- `fix:` 修 bug
- `docs:` 文档
- `refactor:` 重构，行为不变
- `chore:` 构建、配置、依赖

commit 里值得写「为什么」。**这个项目里最有价值的注释解释的是踩过的坑**（比如为什么天箭必须用 20 字节报文、为什么自绘复选框不能用 `BS_AUTOCHECKBOX | BS_OWNERDRAW`），而不是解释代码在做什么。

## Pull Request

提交前自检：

- [ ] `cargo fmt` 已跑过，`cargo clippy -- -D warnings` 无告警
- [ ] `cargo test` 通过；改了换算逻辑的话补了单元测试
- [ ] release 产物体积没有失控（验收线 5 MB，目前约 0.9 MB）
- [ ] 没引入新的依赖，或者能说清楚为什么非加不可
- [ ] 新设备的 `protocol_source` 和 `verification` 都填了

PR 描述里请说明**怎么验证的**。UI 相关的改动附截图最有帮助。

## 发布

打 tag 会触发 CI 构建并自动把 zip 挂到 GitHub Release 上：

```bash
git tag v0.2.0
git push origin v0.2.0
```

Release 里应该写清楚这版新增了哪些设备 —— 用户是靠设备清单决定要不要装的。

## 许可证

MIT。你贡献的部分将以同样的许可证发布。
