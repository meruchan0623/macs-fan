# 生命周期接线与生产身份前置条件

本文记录原有Fake生命周期/身份验收。当前工程另外加入真实SMC只读接线，见readonly-integration.md；它不增加可写后端，也不把本文的1秒预算升级为真实I/O或硬件恢复保证。

原生命周期阶段只修改 helper，未安装服务、提权或写SMC；后续已链接真实SMC只读adapter。0.7新增生产宿主与打包，当前状态见production-host.md和service-management.md。原有 34 项真实 NSXPC 检查继续通过。新增接口是可信原生宿主内部 API，没有增加任何 wire command。

## 原生事件与串行恢复

`IORegisterForSystemPower` 注册 Root Power Domain 的公开睡眠/唤醒通知，`IONotificationPortSetDispatchQueue` 将回调送到与 IPC、timer 共用的引擎串行队列。它仅用于电源通知与确认，不是 SMC 访问接口。注册失败拒绝启动 dry-run 服务，避免声称已有电源事件保护。

- `kIOMessageCanSystemSleep` 只是空闲睡眠询问，立即确认，不撤销租约或否决睡眠。
- `kIOMessageSystemWillSleep` 触发内部 `Tick(Sleep)`，立即撤租并锁止 Acquire/Renew，然后在原生队列上异步观察有界恢复。最多等待 1 秒后确认该电源通知；若未确认恢复，会先记录明确故障，不能把确认系统通知当成风扇已自动恢复。
- `kIOMessageSystemHasPoweredOn` 再次以可信 Sleep tick 确保旧租约已撤销，标记 `wake_requires_new_session`。不创建新引擎、不解锁、不重放旧目标或 token，也不清除 exhausted。当前必须在恢复完成、能力重验与新用户会话授权后，由未来宿主显式重建引擎。
- `NSWorkspaceWillPowerOffNotification`、SIGTERM 与 SIGINT 汇入同一个退出入口。重复通知不会刷新退出 deadline。SIGKILL、进程崩溃和系统强杀无法由进程内 handler 补救。

本机 SDK 的 `IOPMLib.h` 明确要求仅对 CanSystemSleep / WillSleep 调用 `IOAllowPowerChange`；唤醒不得确认。WillSleep 属于不可取消睡眠，未确认会造成系统最多约 30 秒等待，因此本实现不能无限阻塞，也从不调用 `IOCancelPowerChange` 或触发系统睡眠的 API。IOAllow 的返回错误会记录；退出时会先清理仍待确认的睡眠通知，再注销 notifier、销毁通知 port 和关闭电源连接。[Apple QA1340](https://developer.apple.com/library/archive/qa/qa1340/_index.html)，[Apple IOAllowPowerChange](https://developer.apple.com/documentation/iokit/1557064-ioallowpowerchange)

恢复等待使用宿主单调 uptime，正常预算为 1 秒；定时 poll 不阻塞串行队列，因此 core 的 250 ms 重试与最多 3 次写入预算可以推进。所有结果都明确标记 `recovery_scope=tracked_leases_only`，并分别给出 `recovery_confirmed` / `recovery_pending`。Yielded、exhausted 或 deadline 仍 pending 均不算已恢复自动。退出仅在确认时返回 0，否则报告 `shutdown_recovery_unconfirmed` 并返回 74。

这个时间预算不可能抢占一个未来执行器中无限阻塞的 I/O：真实 adapter 必须给每次调用设置可验证的有限超时，并为调度延迟预留预算。当前仅在无硬件 I/O 的 Fake 上验证时序；不能宣称真实风扇已经满足 1 秒恢复保证。

## 固定生产身份模板

`ProductionIdentity.m` 使用源码常量固定客户端 `local.macsfan.desktop` 和 helper `local.macsfan.helper.control`，独立于原程序。生产 Team ID 仍为空，不读取 IPC、Info.plist、环境变量、请求 PID 或宿主凭据。Team 模板输入只允许 10 位大写字母/数字，构建两条 requirement 并以 `SecRequirementCreateWithString` 编译检查：

```text
anchor apple generic and identifier "<固定 bundle ID>" and certificate leaf[subject.OU] = "<已审定 Team ID>"
```

配置完成后，自身仍需通过 `SecCodeCopySelf` / `SecCodeCheckValidity` 满足固定 helper requirement；nil 或编译失败的 requirement 不会变成无约束通过。当前 `--production-check` / `--serve-production` 均明确拒绝未配置 Team，且 `production_channel_available=false` / `hardware_write_enabled=false`。0.7中，production-check只验证身份；serve-production在固定身份、root与requirements全部通过后只创建不可写SMC宿主，不开放可写硬件backend。

模板不是发行身份。真实签名到位后必须对照实际发行 DR 审核 Team、证书链、渠道、hardened runtime 与相关 entitlement，再把客户端 requirement 用于生产 NSXPC 入站校验、helper requirement 用于反向校验。Apple 建议依据实际签名生成的 DR 审核要求，而不是将未经验证的模板当成发行证明。[Apple TN3127](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements)

原有精确 CDHash pin 仍只供本地 dry-run 使用，不能通过 Team 字段或环境切换为生产。0.7普通调试/客户端/配置入口仍拒绝root；仅精确的--serve-production分支在固定身份通过后要求root，且只能创建只读宿主。实际root服务尚未注册或运行。

## 可复现验证与边界

`lifecycle_probe.py` 同时启用 Rust `test-harness` feature 和 native `MF_TEST_HARNESS=1`，单独编译本地测试二进制。只有这个构建包含事件模拟与自动恢复故障注入；普通构建不识别 `--lifecycle-probe`。测试代码直接走与真实回调共享的原生事件映射，不暴露 XPC selector。没有触发真实系统睡眠、唤醒、注销或关机。

13 项检查验证模板语法、同固定 helper ID 的 ad-hoc 签名仍不满足 Apple/Team 要求、非法 Team 输入、未配置生产入口拒绝、环境伪造无法启用生产、睡眠与唤醒锁止、睡眠恢复耗尽后唤醒不清故障、退出成功、三次失败耗尽、等待超时，以及只发送给测试子进程的真实 SIGTERM 成功/失败退出。失败场景保留 manual Fake 状态与未确认标记，没有返回伪造的 automatic。

另有 8 项 Rust 单测、34 项真实 IPC 回归和 native 严格编译。记录在 `lifecycle-verification.json` 和 `verification.json`。原生公开通知已注册，但真实系统事件时序、睡眠期间硬件行为、OS 强杀和真实证书验证均尚未现场验收；未来开放控制前仍必须在专用环境及真实硬件完成这些门槛。
