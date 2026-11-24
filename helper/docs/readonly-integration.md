# 真实只读 helper 接线

2026-11-22 在 Mac16,8 / aarch64 上验证真实只读SMC链路。当前0.9验证34项通过，见[readonly-verification.json](readonly-verification.json)；下方首次32项记录为0.6历史，其原始JSON已保存在outputs/evidence/helper-through-0.8。各轮均没有注册服务、使用root、调用原厂helper或尝试硬件写入。

## 0.6首次接线验证（历史）

路径为 Python 测试驱动 → 临时 ad-hoc 签名客户端 → 双向 CDHash pin 的嵌入式 XPC helper → Rust host → 同一 `SmcAdapter::open_readonly()` 实例。`MFBackend=smc_readonly` 仍要求 `MFDryRun=true`、有效精确客户端 pin 和通过自身签名校验；`MFBackend=smc` 即使重新签名仍拒绝配置。

`open_readonly()` 将私有 profile 永久置空，helper 的 readonly backend 写方法另行直接拒绝。probe 在构建之前核对构造路径，随后才发送用于验证拒绝的 acquire。真实响应保持 `backend=smc_readonly`、`hardware_reads=true`、`hardware_write_enabled=false`。外层 `hardware_write_attempts` 在首次 inspection 前为 null，之后取最近一次 inspection 的适配器 transport 计数，本次为0；`hardware_write_attempts_scope=latest_hardware_inspection` 和 `hardware_inspection_observed_at_ms` 标明范围与观测时间，不是从返回成功与否推断。

`enumerate` 返回真实风扇 `[0,1]`。新增闭合请求：

```json
{"version":1,"command":{"op":"inspect_hardware"}}
```

成功结果的 `result.kind` 为 `hardware_inspection`，`result.inspection` 是请求发生时在同一适配器调用 `probe()` 得到的完整 camelCase `ProbeReport`，含身份、全局键、逐风扇键及其元数据、观测模式、范围、拒绝原因与写计数。它是只读诊断；不能把其中未验证的能力观测当作控制授权。此操作不接受 `fan` 或其他字段，额外字段实测返回 `invalid_schema`。

原有 `capabilities` 继续经过 control-core 的验证门槛。两个风扇均返回 `unverified_capabilities`；合法 TTL、序列和真实范围内 RPM 的 `acquire` 也均返回此错误。probe 在拒绝前后各执行一次 `inspect_hardware`，确认 `hardwareProfileVerified=false`、`hardwareWriteAttempts=0`、外层写计数为0且状态没有租约。没有为了展示能力而降低 core 的许可条件。

计数范围与观测时间也有独立断言：首次 enumerate 的计数和时间均为 null；inspection 后计数为0、时间为非负整数；第二次 inspection 的时间不倒退，scope 始终为 `latest_hardware_inspection`。

本机两个风扇的 min/max 均为 2317/7826 RPM，Mn/Mx/Tg 编码为 `flt /4`；模式观察为 automatic，F0Md/F1Md 原始值为0。FS! 和小写 md 缺失，以结构化 missing_key 保留。Ftst 为 `ui8 /1`、原始0。实际 RPM 随时间变化，不作为固定断言。probe 同时验证了真实错误服务 pin 的客户端拒绝（4102 且 `signing_requirement_failure=true`）和不同真实 CDHash 客户端的服务端拒绝。服务端对错误客户端返回通用 XPC 4097，不能单凭这个错误认定签名检查成功：因此另建显式允许其真实 CDHash 的 smc_readonly 服务，helper保持原始构建和签名运行时选项，仅修改允许 pin 并重新签名；客户端使用与被拒绝样本逐字节相同的 runtime 签名可执行文件，成功 enumerate 和 inspect 真实硬件，并确认写计数为0。该阳性对照排除了客户端自身无法启动 XPC 的假阳性。helper PID 与客户端及 Python 均不同。

运行方式：

```sh
python3 outputs/macs-fan/helper/scripts/readonly_probe.py
```

脚本复用现有 `probe.py` 的实际 bundle、签名及 stdin 客户端接口，但不修改纯 fake 回归。每次建立独立临时 Cargo target 与 bundle，终端显示构建、签名命令和已脱敏响应，JSON 证据包含本次检查、项目输入 manifest、签名后二进制、临时路径、真实响应及时间。`build_input_sha256` 覆盖 helper/control-core/smc-adapter 的全部 `.rs/.m/.h/.inc`、`Cargo.toml`、`Cargo.lock` 和两份 probe 脚本，路径相对 macs-fan，排除 target/docs。clang 编译后及整体验证完成时均重新计算，断言输入未变化；兼容保留 `source_sha256`。`signed_binaries` 记录实际主客户端/helper及反事实客户端/helper的路径、SHA-256 和真实 CDHash。这些是本次项目输入/输出记录，不构成 SDK、工具链、外部依赖或可复现构建的证明。拒绝响应没有租约 secret；若出现意外 token，记录函数也会脱敏。共享 Driver 的8秒等待只约束客户端等待，不是内核 I/O 的硬时限。本次完整命令输出留在 `/tmp/macs-fan-readonly-integration-final.log`。

这次通过只证明受认证的只读硬件链路与拒绝路径可运行。缺少生产签名身份、逐机型已审核写 profile、硬 IOKit 时限及 helper 崩溃/卡住后的独立恢复；不能视为生产控制已就绪，也不能据此启用 GUI 控制按钮。

## 0.9目标更新拒绝复验

新set_target协议加入后，重新构建并执行34项实际跨进程检查。两个风扇没有租约时，即使提供格式正确token与RPM也返回no_active_lease；前后inspection均hardwareProfileVerified=false、hardwareWriteAttempts=0且statuses为空。其他身份拒绝与相同客户端允许反事实仍通过。

这轮实际模式F0Md/F1Md原值为3，显示unknown；范围仍2317–7826。不将0.6阶段观察到的模式0/automatic泛化到当前。所有项目输入编译前后及验收后hash一致；实际签名产物见当前JSON。终端日志为outputs/evidence/validation/helper-readonly-0.9.txt。当前验证ZIP随本轮更新，不等于GUI或生产root服务的控制验收。

## 0.10 单账本宿主回归

当前readonly-verification.json包含36项实际检查与27项项目输入指纹，包含control-runtime接线源。两个真实fan的start_plan请求均因unverified_capabilities拒绝，前后同实例hardwareWriteAttempts为0。模式原值3仍unknown。当前测试bundle归档更新为0.10；历史34项和32项是之前版本证据。
