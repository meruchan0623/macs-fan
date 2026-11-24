# 公开 SDK 与官方依据核对

核对日期 2026-11-22。本机 `xcrun --show-sdk-version` 为 **27.2**，SDK 根目录由 `xcrun --show-sdk-path` 返回。helper 使用 `-mmacosx-version-min=13.0` 编译，实际运行验收只覆盖当前 arm64 macOS 27.2。

| SDK 内相对路径 | 核对内容 |
| --- | --- |
| `System/Library/Frameworks/Foundation.framework/Headers/NSXPCConnection.h:153` | 公开 `setCodeSigningRequirement:`，macOS 13+；设置时机、畸形要求与连接失效语义 |
| `System/Library/Frameworks/IOKit.framework/Headers/pwr_mgt/IOPMLib.h:127` | `IORegisterForSystemPower`、事件确认、run-loop/dispatch queue 接入及释放契约；不包含系统退出事件 |
| `System/Library/Frameworks/IOKit.framework/Headers/pwr_mgt/IOPMLib.h:191` | `IODeregisterForSystemPower` 和后续通知 port/连接释放 |
| `System/Library/Frameworks/IOKit.framework/Headers/pwr_mgt/IOPMLib.h:199` | `IOAllowPowerChange` 仅用于 CanSystemSleep / WillSleep |
| `System/Library/Frameworks/IOKit.framework/Headers/IOMessage.h:156` | WillSleep 常量；同文件第 175 行为 HasPoweredOn |
| `System/Library/Frameworks/AppKit.framework/Headers/NSWorkspace.h:320` | 公开 `NSWorkspaceWillPowerOffNotification` |
| `System/Library/Frameworks/Security.framework/Headers/SecCode.h` | `SecCodeCopySelf`、`SecCodeCheckValidity` 的动态代码与 requirement 验证 |
| `System/Library/Frameworks/Security.framework/Headers/SecRequirement.h` | `SecRequirementCreateWithString` 的语法编译与错误返回 |

官方参考：

- [NSXPCConnection.setCodeSigningRequirement](https://developer.apple.com/documentation/foundation/nsxpcconnection/setcodesigningrequirement(_:))：真实连接 peer 的公开代码要求 API。
- [Apple QA1340](https://developer.apple.com/library/archive/qa/qa1340/_index.html)：睡眠/唤醒注册、事件类型与确认规则。
- [IOAllowPowerChange](https://developer.apple.com/documentation/iokit/1557064-ioallowpowerchange)：确认通知的参数与允许事件。
- [TN3127: Code Signing Requirements](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements)：代码身份、anchor、Team 和实际发行 DR 的审核依据。

以上用于实现与测试选择，不等于真实证书、安装授权或硬件验收。测试没有调用触发系统睡眠/关机的 API，也未复制或依赖原厂 helper 的身份与授权流程。
