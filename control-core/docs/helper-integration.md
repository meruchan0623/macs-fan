# Helper 接入契约与验收

本 crate 实现控制事务和短期租约，当前没有生产 `Actuator`、IPC、认证、ServiceManagement 安装或任何真实 SMC 写入。它没有链接到 Tauri 应用，不能据此启用控制按钮。纯策略仍由 `../../control-runtime/src/policy.rs` 单独负责；策略建议不等于执行成功。上层设计依据为 `../../docs/control-design.md`，原程序静态证据见 `../../../reverse-engineering-report.md`。

## 可调用接口

`Engine<A: Actuator, T: TokenSource>::execute(owner, command, now_ms)` 返回 typed `Result<Response, Error>`。整个引擎必须位于 helper 内的同一个串行执行器中；持锁之后再采集 helper 自己的单调时间。不允许多个引擎共享同一硬件。`status(fan)` 和 `statuses()` 只取内存快照，不执行硬件 I/O；快照是上次确认状态，上层应附上观察时间，不能显示为持续实时保证。

| Command | 请求与成功含义 |
| --- | --- |
| `Enumerate` | 从 actuator 自行枚举风扇，拒绝重复编号；不推断机型可写 |
| `Capabilities { fan }` | 从 actuator 自行读取并校验机型、风扇、范围、步长、模式与编码；客户端不能传能力表 |
| `Acquire { fan, rpm, ttl_ms, seq }` | 取得固定目标的单风扇租约；只有目标与 manual 模式均已读回，才返回 `LeaseGrant` |
| `Renew { fan, token, ttl_ms, seq }` | 校验 owner、token、序列、能力及当前模式/目标后延长租约；不写 RPM，不修改固定目标 |
| `Release { fan, token, seq }` | 释放本人当前租约并请求固件自动控制；只有自动模式得到读回确认才返回 `AutomaticConfirmed` |
| `Tick(reason)` | helper 独立看门狗和可信生命周期事件；返回全部已追踪风扇状态，包括恢复失败 |

`Release` 就是受所有权约束的“恢复自动”命令，没有无条件重置其他程序风扇的入口。改变固定 RPM 需先成功释放，再显式取得新租约。协议不接受任意 SMC key、原始数据、selector、路径、命令或请求 PID。

`Owner::from_authenticated_transport` 只包装外部已认证的 opaque 32 字节身份，它自身不实施认证。helper 必须从系统 audit token / 认证连接建立 owner，并保证每个连接实例唯一；不能从请求体、PID、路径、用户自报签名或 UI 配置构造身份。`TickReason::Disconnected(owner)` 同样由连接生命周期产生，不是客户端自报。客户端只允许枚举、能力、获取、续租、释放五类 wire 请求；`Tick` 全部保留给 helper 内部。

`TokenSource::next_token` 必须接入系统 CSPRNG，每次产生全新 32 字节。crate 不提供生产随机实现。引擎拒绝全零及本实例曾出现的 token，最多记住 65,536 个已签发 secret；达到上限会拒绝新租约，需安全恢复完所有风扇后轮换引擎。此防重用检查不能把不可信 PRNG 变成 CSPRNG。token 只发送给已认证的持有者，不写日志；相关 `Debug` 已隐藏字节。错误与状态快照不包含 token 或 owner。

## 能力与事务

Actuator 的 `enumerate/capabilities/read_state/write_target/write_mode` 都由它自身访问硬件；UI 范围不能进入信任边界。`validated=true` 由经审核机型表与现场读取共同产生，至少包括风扇计数、编号、键允许列表、编码/尺寸、模式语义及零 RPM 许可。未知机型、未知模式、未知编码、能力编号不匹配或无法在自动模式安全暂存目标时全部拒绝。共享模式掩码必须由 adapter 保留其他风扇位。

范围、步长与请求必须有限、非负、有序，步长大于零；请求必须精确位于步长格点。fpe2 限定到无符号 16 位的四分之一 RPM 编码，Float32 要求往返转换无损；范围端点与步长也必须可表示。不静默夹紧、舍入或量化。`HardwareState.target_rpm` 是控制目标，不是瞬时实际转速。读回用精确目标值比较，adapter 必须提供真实解码结果，不能返回缓存的请求值。

获取顺序固定：验证请求/TTL → 自行读取枚举与能力 → 确认当前 automatic → 生成 token → 在 automatic 下写目标 → 读回 automatic 与目标 → 写 manual → 再次读回 manual 与目标。这样不会先进入带旧目标的 manual。若机型不支持安全暂存，先拒绝，不能擅自调换顺序。任何写入错误，包括可能已改变硬件的 timeout，或读回错误/失配都会撤销 token 并进入自动恢复；事务失败时即使恢复成功，原请求也仍返回故障。

成功切回 automatic 后不恢复旧的手动目标字节；经验证的 adapter 必须保证自动模式确实由固件决定实际转速。Acquire 在模式尚未由本引擎写入前若观察到 manual，判定外部接管，直接让出，不继续写模式。

## 租约、故障与生命周期

每个风扇同时只有一个 owner。TTL 必须为 1–5,000 ms，deadline 使用 checked 加法；`now >= deadline` 即到期。首个序列必须大于零，后续每次 renew/release 严格大于上次成功序列，不允许回绕。达到 `u64::MAX` 后等待到期或可信生命周期撤销。旧 token 永远不能续租新会话。恶意 owner、token、TTL、序列和 RPM 请求不撤销仍然健康的其他租约。

每条命令先处理租约到期。renew 和 periodic tick 都重新读取已知能力与当前模式/目标，能力变化、读失败、无效读数立即撤销租约；已有租约上的能力查询失败也触发恢复，无法靠下一次成功 renew 掩盖。有效的外部 automatic、或不同的有限 manual 目标，表示已失去控制：进入 `Yielded`，不再写目标、不再写模式、不自动重获控制。已确认租约的恢复始终先检查竞争，包括能力或读取故障触发的首次恢复；不能按故障类别跳过检查。只有新的显式 Acquire 且硬件已经 automatic 时才可建立新会话。

以下可信 tick 立即撤销相应租约：

- `Disconnected(owner)`：对应认证连接失效、客户端退出/崩溃。
- `ControlFault(fan)`：采样质量、策略熔断或上层控制 watchdog 故障；固定 RPM 核心本身不分析温度。
- `Sleep` / `Shutdown`：全部租约；之后锁止 Acquire/Renew，需恢复确认完成后重建引擎、重读能力及重新取得用户同意。唤醒不复用旧目标。
- 时间倒退：全部租约撤销，永久锁止新手动控制。待恢复完成、时钟源修复后新建引擎；旧租约不能变成新时钟纪元的有效租约。

自动恢复首次立即尝试；每次写 automatic 都检查返回码并读回模式，包括写失败时也尝试读回。成功写入且读回 automatic 才能立即确认成功。如果写返回错误但已部分切到 automatic，首轮仍报告未确认恢复；后续独立读到 automatic 可确认已经恢复。生命周期撤销先检查外部接管，已经 automatic 时直接确认，无需重复写入。

恢复明确区分已确认租约与尚未确认的 Acquire 事务。前者以已确认目标作为竞争基线；后者仅首轮允许直接尝试恢复 automatic，并以故障现场首次读到的有限 manual 目标作为基线，而不是以从未确认生效的请求值判断竞争。例如请求 3000、失败现场残留 1500，下一轮仍是 1500 时继续有界恢复，不能误报外部接管。若故障现场读不到目标，使用恢复读回或后续 guard 的首个实际有限 manual 目标建立基线；基线一经建立就不被之后的读数覆盖。后续目标不同于基线才进入 `Yielded`，不再争抢。原始事务错误始终保留，建立基线不表示目标控制已成功。

恢复最多尝试 **3 次 automatic 写入**，后续两次至少间隔 250 ms，由 periodic tick 驱动。失败显示 `Phase::Recovering { attempts, next_retry_at_ms, exhausted, last_error, cause }`。第三次失败、或重试 deadline 溢出后，不再写入，保留 exhausted 状态并拒绝 Acquire/Renew。该状态不能通过客户端请求重置；由服务运维处理并独立确认自动恢复后才能轮换引擎。时钟倒退会重定位尚未结束的重试 deadline，保留已有次数，不刷新重试预算。`Yielded` 和 `Recovering` 都不能显示“已恢复自动”。

## 接入步骤

1. 在独立签名 helper 中实现经审核的 `Actuator`。逐方法设置有限 I/O 超时，禁止复用原厂 helper、随意 key 或信任 UI 能力。先在 Fake / 专用硬件环境验证，不在普通开发运行中开启写入。
2. 接入系统 CSPRNG、系统提供的客户端认证与服务反向认证。认证完成后分配连接 owner，严格 decode 有限 typed wire 请求并拒绝未知字段/变体；不把 owner、helper 时间或 host tick 暴露成客户端参数。
3. 创建唯一引擎和串行队列。独立于 UI 心跳运行 periodic tick，例如每 100 ms；在队列取得执行权后读取单调时间。I/O 必须足够有界，执行完成后以新时间再 tick，避免耗时事务的旧时间被当作当前。TTL 是逻辑有效期，不是本 crate 已证明的硬件恢复时限。
4. 连接断开、电源事件、服务更新/退出、采样/策略失败转为上述可信 tick。Sleep/Shutdown 后持续驱动恢复，等待确认或明确 exhausted；不能直接 drop 引擎并声称恢复成功。Rust `Drop` 不执行隐藏的硬件恢复。
5. 将每次结果与 `statuses()` 一起传给 UI/审计层；read 查询返回错误时也可能已触发恢复。审计记录时间、风扇、动作、原因及恢复结果，不记录 bearer secret。`LeaseGrant` 表示目标和模式已确认，实际转速收敛需另设观察时间窗。
6. 维持应用控制入口关闭，直到认证、安装、硬件、热安全与独立故障恢复全部达到下列验收。策略预览不能当作控制成功。

## 已验证与仍待验证

独立测试以 Fake 注入枚举/能力/读写错误、部分成功、伪成功、读回失配、无效编码与浮点值，覆盖 owner/token/fan 绑定、重放、严格序列、最大 TTL、到期边界、时间回退与整数溢出、有界恢复、竞争让出、断线/电源事件，以及共享锁下八个并发请求只有一个取得同风扇租约。长串行序列持续检查健康 Fake 的 manual 模式必须对应未到期有效租约。

这些测试不证明真实机型允许写入，也不证明真实 helper 崩溃后能够恢复。开放前仍必须完成：

- 独立身份签名/公证、ServiceManagement 安装与 XPC 双向认证；伪造、替换、越权客户端拒绝。
- 多机型模式/目标真实往返、共享掩码互不干扰、实际 RPM 收敛与热保护；未知机型和未知尺寸/编码始终拒绝。
- 断连、进程强杀、helper 强杀、睡眠/唤醒、采样冻结、I/O 阻塞/错误、读回失配、自动恢复失败；测量有界恢复时间。
- 独立于此进程的恢复设施或经硬件验证的固件自动超时。引擎未运行、死锁、进程崩溃及机器休眠期间，这个纯库无法主动写硬件，不能以短 TTL 替代该设施。

硬件通常不带租约 owner 标记；读 mode/target 无法识别另一控制器写入相同值，也无法消除读写间竞争。已确认持有期间发现变化会让出；本引擎自己尚未确认的事务中遇到失配，按部分失败执行自动恢复。此边界必须通过 helper 的平台协调及真实硬件对抗测试进一步约束，不能宣称通用硬件互斥或生产安全已经成立。
