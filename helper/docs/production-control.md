# 0.16 控制构造与 host 准入

生产 `MFServeProduction` 继续调用 `mf_helper_create_smc_readonly()`。新增无参数 `mf_helper_create_smc_control()` 只完成源码接线；本轮未调用真实控制构造、未注册服务、未做 SMC 写入。内部 `PRODUCTION_PROFILES` 仍为空。

`SmcAdapter::control_admission()` 只读地检查内部编译 profile、完整库存、每个风扇的 identity、精确 metadata、范围及模式一致性，返回封闭的 `NoReviewedProfile / ProfileValidationFailed / Admitted`。结果是构造时观察，不是可用来写入的 Admission，也不开放 profile、key 或调用者验证断言。后续每次实际 target/mode 写仍执行 adapter 原有完整校验与 transport 的 real-registry 验证。

构造成功且 admitted 时，私有 `Backend::SmcControl` 委托全部 Actuator 操作；多风扇委托在 Fake 的 fan-0 限制之前执行。原 `GuardedBackend` 在 target/Manual 前检查期限，Automatic 恢复保留原路径。同步 IOKit 仍不能被 timer 抢占。

无 profile 或验证失败时，构造器通过 `into_readonly(self)` 永久清除实例 profile，同时装入从不转发写操作的 `Backend::SmcReadonly`。后续硬件读取恢复也不能让该实例重新获得能力，capabilities/inspection 不再声明 verified。显式 `open_readonly()` 原有清表保证不变。打开失败使用真实 Unavailable backend，保留结构化 `backend_open_error`，不回退 Fake。成功打开后的候选采样仍由独立 reader 初始化；候选为 Unverified，不提供 sensor-plan 控制来源。

Host 的私有 `HostExecution` 在构造时选定，wire、inspection、plan、睡眠及恢复均不能改变。单个 `attach_execution` 生成 backend、dry_run、hardware_write_enabled 和 plan 顶层 control；成功、业务失败与最终刷新均使用它，刷新不会复制旧 control。

| backend | dry_run | write | execution | admission |
|---|---|---|---|---|
| smc_readonly | true | false | readonly | readonly_policy |
| smc_readonly | true | false | readonly | no_reviewed_profile |
| smc_readonly | true | false | readonly | profile_validation_failed |
| unavailable | true | false | readonly | backend_open_failed |
| unavailable | true | false | readonly | backend_unavailable |
| smc_control | false | true | profile_gated | admitted |
| fake | true | false | test_fake | test_only |

control 恰含 `version: 1`、execution、admission。所有已解析的 plan_status/start_plan/renew_plan/stop_plan 回复均附加该对象。inspection 的宿主 flags 如实表示执行种类，不附加 control，也不授予计划授权。

start_plan 在既有维护/过期审计之后、busy/seq/TTL/策略检查及 RNG/token/授权/写入之前执行 Host 准入检查。五种拒绝原因精确等于 admission。renew/stop 继续使用原 owner/token/seq 规则；无合法计划时仍返回 no_active_plan。所有 direct core 命令最终受实际 backend 写拒绝约束。

验证仅使用编译期 Mock 与 Fake：adapter 26 项、helper 57 项单测通过；两 crate 的 all-targets clippy `-D warnings` 通过，helper 同时启用 all-features。测试包含完整库存及第二风扇漂移、永久降权的 capabilities/inspection/全部写拒绝、第二风扇 Actuator 委托、期限及 Automatic 恢复、七组合 wire、五类 Start 拒绝零 RNG/token、成功及失败 start/renew/stop 的一致 fingerprint、最终刷新不信任旧 control、FFI 无效 handle/panic 不提供 control。Host 的 admitted wire fixture 仅在 Rust `cfg(test)` 内使用 Fake engine，不提供生产 runtime override；真实写准入正向仅接 Mock transport，未加入 production registry。

命令、输出与文件 SHA-256 见工作区 `work/control-host-0.16/`（源码目录之外）。这些证据证明离线软件接线，不证明硬件恢复、物理转速、发行签名、root listener 或真实系统电源事件。随后完整非特权Fake XPC集成及36项真实SMC只读回归通过；真实control构造仍未执行。实际结果见上级交付状态与独立评审。
