# 控制事务与租约核心

独立、无外部依赖的 Rust crate。提供固定 RPM 控制的验证、串行事务、读回确认、短期租约和有界自动恢复；不重复 `policy.rs` 的温控曲线。

**当前仅有 generic 引擎和测试用 Fake actuator。未接入应用、真实 helper、SMC 写入、安装器或权限服务，不代表生产控制已开放。**

```sh
cargo test --offline --manifest-path outputs/macs-fan/control-core/Cargo.toml
cargo clippy --offline --manifest-path outputs/macs-fan/control-core/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path outputs/macs-fan/control-core/Cargo.toml -- --check
```

从项目根目录运行。接口、接入步骤与剩余硬件验收见 [helper-integration.md](docs/helper-integration.md)。
