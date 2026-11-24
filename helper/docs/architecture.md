# 身份校验、协议与执行边界

这是一条已经实际连通的 **dry-run** 通道：临时测试客户端 → macOS NSXPC → Objective-C 认证入口 → Rust JSON adapter → 现有 `control-core` → Fake / Unavailable / 永久只读SMC适配器。实际验证载体和生产入口均为只读；新增受profile约束的执行器构造未在真实硬件启用，亦未安装特权服务。测试成功不表示本机风扇已受控。

0.16源码已接GUI显式应用/停止、持久认证客户端、Coordinator单账本及封闭生产能力握手；Preparing/Running/Stop在隔离Fake链验证。新增SmcControl构造只接受内部profile完整验证，失败后永久降权，真实生产入口仍readonly。宿主独立候选采样已接，但全部Unverified且不能准入；真实写profile/校准表仍为空。桌面契约见[control-bridge.md](../../docs/control-bridge.md)，构造边界见[production-control.md](production-control.md)，候选边界见[candidate-sampling.md](candidate-sampling.md)。

## 组件与信任边界

| 文件 | 职责 |
| --- | --- |
| `native/Helper.m` | 公开 NSXPC selector、连接代码要求、原生会话 owner、断线事件、串行队列及独立 100 ms timer；也包含仅供验证的本地客户端驱动 |
| `native/Bridge.h` | 内部 C ABI；owner buffer 只由原生会话生成，不是 wire 参数 |
| `src/plans.rs` | 0.10计划schema、连接授权/心跳截止和单账本Coordinator接线；详见host-plans.md |
| `src/lib.rs` | 4 KB schema、typed commands、系统 CSPRNG token、单调时钟、真实 `control-core` 适配、响应、内存后端与SMC只读后端 |
| `scripts/probe.py` | 构建临时 bundle、精确 source pin、真实跨进程 IPC 和失败反事实验证；打印命令及结果，不打印租约 secret |
| `native/ProductionIdentity.m` | 固定独立 bundle ID、受限 Team ID 模板构建及实际 SecCode 自身校验；当前 Team 未配置，生产入口拒绝启动 |
| `native/LifecycleTests.inc`、`scripts/lifecycle_probe.py` | 仅双编译开关启用的本地生命周期与故障测试，不是 IPC 协议 |

每条已送达 selector 的消息，先由系统 NSXPC 签名要求验证，再进入会话方法。owner 是 `SecRandomCopyBytes` 产生的 32 字节连接实例标识；一条尚未认证的连接可以分配内存，但不能在签名检查之前进入 Rust 或取得租约。interruption/invalidation 都把同一会话关闭并产生内部 `Disconnected(owner)`，close 可重复调用但只撤销一次；关闭后的请求不进入引擎。

原生全局串行队列与 Rust host mutex 保护一个引擎。请求同步交接到串行队列，提供每条连接的背压，不把每个请求另外追加到无界应用队列。Rust 在取得锁后读取自己的 `Instant`；客户端不能提供时间或发送 tick。独立 timer 通过内部 ABI tick，与 UI 是否续租无关。处理完命令后再次读取时间并 tick；若刚返回的 lease 已被撤销，不再把它发送为成功 grant。

## 真实签名校验的选择

本机公开 SDK 的 `NSXPCConnection.h` 没有公开完整 audit token 属性。实现没有调用私有 `auditToken` KVC、没有将连接 PID 转成可竞态的代码身份，也没有把 `getpeereid` 当作签名认证。采用 macOS 13+ 官方 `setCodeSigningRequirement:`，在 `resume` 前设置，由系统对真实 peer 消息进行要求校验。客户端也设置服务端要求，避免连接错误 helper。Apple 文档说明此 API 约束真实连接对端，要求不匹配时连接失效；畸形要求可能抛出异常，因此先用 `SecRequirementCreateWithString` 编译校验，再设置要求。[Apple API 文档](https://developer.apple.com/documentation/foundation/nsxpcconnection/setcodesigningrequirement(_:))

本地dry-run入口只接受 `cdhash H"…"` 形式的精确调试 pin，拒绝 `true`、宽泛要求、畸形文本、缺失配置及任何未知 backend（允许fake、unavailable、smc_readonly）。该测试服务bundle必须声明 `MFDryRun=true`，并通过 `SecCodeCopySelf` / `SecCodeCheckValidity` 自身动态签名校验后才创建引擎或listener。实测先验证一份完整签名的服务配置，再修改 `Info.plist` 的合法backend或合法pin **且不重签**，均拒绝启动。把backend设成 `smc` 或清除dry-run标记也拒绝。固定生产入口独立使用下述Apple/Team requirements，不接受这些调试配置。

精确 CDHash 仅能标识这一份已构建代码，ad-hoc 没有可信发行者身份，重建后必须更新 pin。这不能作为生产 Team ID、证书链、签名更新策略或安装授权的替代。Apple 的要求说明也区分代码标识字符串与签名身份，并说明 ad-hoc 身份随具体代码版本变化。[Apple TN3127](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements)

没有有效生产签名配置时不存在写通道；当前固定 Team ID 源码常量为空，`--production-check` / `--serve-production` 都拒绝，wire 和环境均不能填充它。smc_readonly只通过open_readonly构造，并在helper包装层拒绝写方法；新增SmcControl只可由内部已准入构造选择，当前生产入口不使用它且profile表为空。未来需从实际发行签名生成、审核双向 designated requirement，绑定独立 bundle ID 与 Team 身份；不能将客户端传入的任意 requirement 作为授权。测试驱动的服务 pin 命令行参数只适用于这一份不会写硬件的验证工具。新增模板与生命周期详见 [lifecycle-production.md](lifecycle-production.md)。

## Wire schema

唯一 selector 为 `request:(NSData *)payload reply:…`，payload 是 UTF-8 JSON，最大 **4096 字节**（包含空白）。原生入口与 Rust 都检查大小；这限制应用协议输入，不代表能在 Foundation 解码归档之前阻止所有内存分配。

```json
{"version":1,"command":{"op":"acquire","fan":0,"rpm":3000,"ttl_ms":5000,"seq":1}}
```

| op | 允许字段 |
| --- | --- |
| `enumerate` | 仅 `op` |
| `inspect_hardware` | 仅 `op`；SMC只读backend返回实际ProbeReport，其他backend返回not_supported |
| `capabilities` | `op, fan` |
| `acquire` | `op, fan, rpm, ttl_ms, seq` |
| `renew` | `op, fan, token, ttl_ms, seq` |
| `set_target` | `op, fan, token, rpm, ttl_ms, seq` |
| `release` | `op, fan, token, seq` |

envelope 只有 `version, command`。所有未知字段、未知操作、重复字段、重复 op、错误类型、多余 JSON、非法 UTF-8、非标准 NaN/Infinity 与超范围整数都会拒绝。没有 owner、PID、audit token、Team ID、now、模式原始字节或内部生命周期命令。

fan 是 u16；ttl/seq 是精确 JSON u64 整数，不能使用字符串、小数或指数表示法。版本目前只有 1。core 进一步约束首个 seq > 0、后续严格递增、TTL 1–5000 ms、RPM 编码/范围及租约身份。token 恰为 64 个小写 ASCII 十六进制字符。消息以内存中的原始 NSData 进入 Rust，不经过 Foundation 浮点数重编码，因此 `18446744073709551615` 能精确验证；未来 JavaScript 客户端必须避免先把大序列转换成有损的 Number。

响应包含 `ok`、typed result 或固定错误 code，以及当前内存状态快照。token 只包含于成功的 lease reply，不进入状态、错误或日志。`dry_run=true` 和 `hardware_write_enabled=false` 始终保留，Fake 的 `automatic_confirmed` 只说明内存 actuator 已读回自动。Unavailable 返回 `actuator_unavailable`，不会把失败编码成零风扇或成功。

`helper_process_id` 只用于诊断跨进程事实，绝不用于认证。`watchdog_ticks/watchdog_expirations` 只由内部 timer ABI 推进；测试据此确认是在下一次请求之前完成到期撤销，避免把查询命令自身的到期处理当成独立看门狗证据。本地测试客户端的 `action/connect/disconnect/payload_base64` 驱动协议只读 stdin，既不是 helper wire schema，也不能成为 owner 来源。

## 实测证据与反事实

`probe.py` 在用户临时目录生成未安装的测试 `.app` 和嵌入式 `.xpc`。NSXPC 按需启动另一个非特权进程，返回的 helper PID 与客户端、Python 驱动均不同。每次重跑都构建、签名并读取新 CDHash，避免用旧构建的 pin 验证新代码。

34 项检查覆盖 15 种畸形/伪造请求、空请求的 dry-run 标记、4096/4097 字节边界、TTL、序列零值/重放/u64 最大值、跨连接 owner 冲突、token 编码、能力与 release、断线撤销与旧 token 失效、真正由 timer 驱动的到期、Unavailable、无配置及错误配置拒绝。

身份检查使用相同 bundle ID 和相同行为的另一份签名代码（改变签名 runtime flags，得到不同真实 CDHash）。原 allow pin 拒绝其请求；只在另一个临时服务显式允许这个实际 hash 后，同一客户端能够连通。此反事实排除了“错误客户端本身无法启动 XPC”造成的假阳性。当前系统服务端拒绝客户端时返回通用 XPC 4097；客户端拒绝错误服务 pin 则返回 4102 / code-signing requirement failure。测试不把所有连接错误笼统当成特定签名错误码。

签名配置篡改测试分别修改合法的 `MFBackend` 和 `MFClientRequirement`，不重签，均在创建 listener 前拒绝；宽泛/畸形要求、未知 backend 和 production 标记测试则先重签对应测试 bundle，证明是配置策略拒绝，而不是仅靠损坏签名拒绝。

## 尚未完成的生产工作

本模块尚未注册或运行系统服务；GUI生产NSXPC客户端和固定只读生产宿主已落盘，但固定Team为空，实际发行认证会话未建立。详见 [control-bridge.md](../../docs/control-bridge.md)、[production-host.md](production-host.md) 和 [service-management.md](service-management.md)。生产入口已调用 `mf_helper_create_smc_readonly`：成功open后构造SmcReadonly执行后端与inspection；仅open失败才保留Unavailable并报告backend_open_error，不是遗漏真实只读后端接线。

真实写入另缺生产可写执行构造及客户端能力握手：helper拒绝SmcReadonly写方法并固定dry_run/write=false，客户端生产开始门槛不放行且拒绝write=true回复。独立TemperatureReader虽已有候选读取证据，真实host仍保留unavailable sampler，只有Fake获得本地控制样本准入。真实候选接线、校准准入和生产profile必须分开，不能把读数合理或目录匹配当作控制许可。

外部条件还包括独立发行身份与实际双向requirement、公证、ServiceManagement管理员批准及安装/升级/卸载现场验收，以及逐机型写入/模式读回、真实睡眠/唤醒和helper强杀后的独立恢复证据。正常退出已在同一串行队列等待最多1秒，确认追踪租约恢复才exit 0，未确认则保留故障并exit 74；这只在Fake/Unavailable上验收，不能推广为真实硬件退出恢复保证。`Instant`的睡眠计时语义也不能替代系统电源事件处理。当前要求与条件统一见 [原始目标账本](../../../requirements-matrix.md)。

当前没有覆盖真实硬件执行器失败、root/跨用户连接、发行签名替换、调试注入抵抗、服务升级中断或全部 OS 版本。原生配置拒绝和 NSXPC 认证是已实现的可验证进展，不能据此启用主应用控制按钮；后续仍需按上层控制设计完成完整的硬件与权限验收。

## 真实SMC观察路径

`inspect_hardware` 不经由控制能力验证绕过准入，而是独立只读观察。原core capabilities仍要求verified；observed min/max只表示读取结果，不能授予控制权限。同一Arc/Mutex adapter由host串行访问，请求时逐键读取；inspection含每键错误，ok表示成功生成观察报告，不保证全部硬件项正常。构造失败保留smc_readonly身份、hardware_reads=false和结构化backend_open_error，不能回退Fake。

外层hardware_write_attempts首次为null，之后是最近真实inspection的计数；scope和单调观察时间明确标示范围。请求可能进行同步IOKit读取，独立timer不能抢占这些调用；250ms分类仅在调用返回后执行，1秒生命周期预算也无法中断卡住的调用。生产控制必须另行解决进程隔离和独立恢复。

## 历史接入阶段

以下是相应版本当时的状态；其中“尚未进入GUI/生产宿主”的描述已被后续接线取代，不是0.14现状。

### 0.9 同租约目标更新

闭合set_target请求映射到control-core的SetTarget事务，必须已有同一连接owner、正确token与递增seq，不能凭请求创建租约。验证能力和当前归属后更新目标，保留token；只有手动模式/目标读回确认才提交新目标和期限。失败恢复区分原目标、尝试目标和其他控制器目标。独立control-runtime协调固定/传感器计划，但尚未进入生产宿主或GUI控制命令。

真实非特权Fake XPC验证38项通过，包含跨连接更新拒绝、同token读回新目标、重放与任意key注入拒绝。真实SMC只读验证另加入无租约set_target拒绝，拒绝后逐实例inspection写尝试仍0。Fake目标更新不是硬件调速，当前SMC profile依旧不可写。

### 0.10 宿主计划

Coordinator已取代HostState的独立Engine字段，direct命令和计划共用账本。start_plan/renew_plan/stop_plan/plan_status及宿主timer接线见[host-plans.md](host-plans.md)。温度计划因缺本地可信来源明确拒绝，GUI尚未接入应用计划，生产仍只读。
