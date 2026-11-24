# 生产 Mach service 宿主

`--serve-production` 已接上独立 LaunchDaemon 可用的 Mach listener，服务名固定为 `local.macsfan.helper.control`。它只构造 `mf_helper_create_smc_readonly()`，没有写后端选择器。源码中的生产 Team 仍为空，当前分发的二进制始终在身份前置校验拒绝，不能启动该 listener。

启动顺序为：固定 helper bundle ID / Apple generic anchor / 固定 Team 的自身签名验证 → 有效 UID 必须为 root → 读取无外部参数的固定 requirements → 构造 SMC 只读宿主和电源观察 → 创建固定 Mach listener → 恢复 listener 并保持 run loop。环境、Info.plist、wire、命令行及本机账户凭据均不能配置 Team、requirements、backend 或 Mach 名称。生产入口只接受一个 `--serve-production` 参数；额外的 fake/backend/pin 参数不能进入生产分支。

`ProductionIdentity.h` 提供只读访问器：`MFProductionClientBundleIdentifier()`、`MFProductionHelperBundleIdentifier()`、`MFProductionMachServiceName()`、`MFProductionConfiguredTeamIdentifier()`、`MFProductionConfiguredRequirements()`。最后一项只有固定 Team 已合法配置才返回 `client` / `helper` 两条 requirement；当前返回 nil。接受 Team 参数的模板函数已改为实现文件私有，只供固定常量与编译门控的语法 fixture 使用。

生产 listener 使用 `setConnectionCodeSigningRequirement:` 在建立 session 前拒绝错误客户端；每个被接受的 NSXPCConnection 在 resume 前再次设置固定 client requirement，约束每条消息的实际对端。现有本地 dry-run CDHash pin 路径保持独立，所有 debug/client/config/fake/test 入口仍拒绝 root。生产客户端必须用固定服务名及 `NSXPCConnectionPrivileged` 连接，并在 resume 前设置固定 helper requirement；服务名本身不证明服务身份。本次未实现 GUI 的生产连接器。

`--production-check` 只检查自身身份：`identity_check_only=true`、`production_listener_resumed=false`，身份有效才返回 0，不创建引擎或 listener。生产 listener 的成功状态为 `production_readonly_listener_resumed` / `production_listener_resumed=true`，同时始终 `production_control_ready=false` / `hardware_write_enabled=false`。该启动日志不代表 launchd 安装正确、端到端 channel 已验收、SMC 读取成功或风扇控制已就绪；真实只读结果仍需客户端请求读回验证。

LaunchDaemon 沿用 IOKit 睡眠/唤醒通知、同一串行引擎队列、100ms watchdog 和有界退出恢复；SIGTERM / SIGINT 进入原有退出入口。daemon 不调用依赖用户 GUI 会话的 NSWorkspace 关机通知，launchd 的退出信号是系统关闭时的宿主退出路径。进程强杀和阻塞 I/O 的原有边界保持不变。

本机 Xcode SDK `Foundation.framework/Headers/NSXPCConnection.h:176` 明确支持 launchd Mach listener；`:184` 表明 delegate 为 weak，因此生产 listener 与 delegate 使用 `objc_precise_lifetime` 持有至 run loop 结束。`:211` 的 listener requirement 在委派前过滤新连接，`:153` 的 connection requirement 对不匹配的新消息使连接失效。run loop 注册一个本地 Mach port，避免无输入源时空转。

`python3 helper/scripts/production_host_probe.py` 在普通 macOS 用户下严格编译普通宿主，使用会立即退出的 bridge stub，验证当前 Team 缺失、环境/已签名 plist 伪造、debug 入口拒绝模拟 root、身份检查与实际启动分离，以及模拟身份和 UID 通过后只到达只读构造器。模拟 root 不改变进程 UID，模拟身份不进入实际监听，所有 fixture 仅在临时目录。15 项检查通过；它们不构成真实 Apple 证书、root、服务安装或 Mach channel 正向验收。

另行运行 `lifecycle_probe.py` 的 13 项身份/生命周期回归与 `probe.py` 的真实本地 dry-run XPC 回归。没有注册服务、执行 root 操作、触发真实系统睡眠或写入 SMC。正式 Team/签名、LaunchDaemon 注册、真实双向认证及真实电源事件仍需具备发行身份后的现场验收。
