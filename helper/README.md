# NSXPC helper dry-run

已连通的原生 NSXPC 入口、严格 JSON 协议适配器和 `control-core` 租约引擎。当前后端包括内存 Fake、明确失败的 Unavailable，以及不可升权的真实 SMC 只读适配器。**没有可写硬件后端、安装器或提权模式**，也未接入主应用。生产身份仅有未启用的固定源码模板；全部 wire 响应标记 `dry_run=true`、`hardware_write_enabled=false`；`hardware_reads` 区分真实只读适配器与内存模拟。

helper 最低 macOS 13，使用公开的 NSXPC 逐消息签名要求校验；GUI 的 macOS 11 最低版本可以保持不变，未来控制功能单独要求 13+。本机编译及实际 IPC 验证为 arm64 / macOS 27.2，不代表已在 macOS 13 真机验收。

从项目根目录运行：

```sh
CARGO_TARGET_DIR=/tmp/macs-fan-helper-verify CARGO_INCREMENTAL=0 MACOSX_DEPLOYMENT_TARGET=13.0 cargo test --offline --manifest-path outputs/macs-fan/helper/Cargo.toml
CARGO_TARGET_DIR=/tmp/macs-fan-helper-verify CARGO_INCREMENTAL=0 MACOSX_DEPLOYMENT_TARGET=13.0 cargo clippy --offline --manifest-path outputs/macs-fan/helper/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path outputs/macs-fan/helper/Cargo.toml -- --check
python3 outputs/macs-fan/helper/scripts/probe.py
python3 outputs/macs-fan/helper/scripts/lifecycle_probe.py
```

`probe.py` 构建临时 `.app/Contents/XPCServices/*.xpc`，只用本地 ad-hoc 签名和精确 CDHash pin，启动非特权双进程，通过真实 NSXPC 检查身份拒绝、断线撤租、看门狗和协议边界。它不调用 `launchctl`、不修改钥匙串或证书、不写 `/Applications` 或服务目录、不触碰 SMC，且拒绝作为 root 运行。临时目录保留用于检查；没有将其安装成后台服务。

已连接公开 IOKit 睡眠/唤醒通知、NSWorkspace 退出通知与 SIGTERM/SIGINT。睡眠撤租并锁止，唤醒要求新会话；退出最多等待 1 秒确认追踪租约恢复，未确认时返回非零，不假报自动。事件模拟和故障注入只存在于双编译开关启用的本地测试二进制，未暴露到 IPC，未触发真实系统睡眠。

2026-11-22 验证：11 项 Rust 测试、原有 34 项真实 IPC 回归、13 项生命周期/身份检查、native `clang -Wall -Wextra -Werror`、clippy 和 fmt 通过。记录见 [verification.json](docs/verification.json) 和 [lifecycle-verification.json](docs/lifecycle-verification.json)，实现与剩余门槛见 [architecture.md](docs/architecture.md) 与 [lifecycle-production.md](docs/lifecycle-production.md)。


## 真实只读集成

`python3 outputs/macs-fan/helper/scripts/readonly_probe.py` 独立构建非特权临时测试bundle，验证NSXPC→Rust核心→同一个AppleSMC只读实例。`inspect_hardware` 每次读取元信息/模式/目标；没有新增任意SMC键或写操作。核心capabilities与acquire继续拒绝未验证的能力。只读构造器不可升权，helper包装层也直接拒绝两个写方法。

完整结果见docs/readonly-integration.md和readonly-verification.json。写尝试计数来自该实例command6发送路径，不是整机所有程序的全局审计；第一次inspection前为null，之后只代表最近一次inspection，附观察时间。同步IOKit阻塞可能拖住串行队列，因此这些读取结果不能证明硬截止、看门狗时限或崩溃后恢复。
