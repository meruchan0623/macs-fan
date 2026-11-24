# 固定生产服务的注册边界

`src-tauri/native/ServiceManager.m` 提供无参数的 `mf_service_status`、`mf_service_register`、`mf_service_unregister`。每次返回由 native 分配的 UTF-8 JSON，调用者负责通过 `mf_service_free` 释放。服务名、Team、要求字符串、plist 名称和可执行路径均不接受 UI 或 IPC 输入。

仅 macOS 13 及以上尝试运行时加载系统 ServiceManagement framework，并取得 `SMAppService.daemonServiceWithPlistName:`。macOS 11/12 返回 `unsupported / macos_13_required`。没有静态 `SMAppService` 类引用，也不需要硬链接新 framework；GUI 的最低系统版本仍为 11。

待正式签名打包时，固定安装结构为：

```text
Macs Fan.app/
  Contents/Library/LaunchDaemons/local.macsfan.helper.control.plist
  Contents/Library/LaunchServices/MacsFanHelper
```

plist 的源模板位于 `helper/packaging/local.macsfan.helper.control.plist`。模板的 `BundleProgram`、两个参数和单一 `MachServices` 均被完整比较；额外字段也会拒绝。此文件是打包输入，本模块不会把它写入 `/Library/LaunchDaemons`，也不会调用 `launchctl`、授权安装器或 sudo。

注册前必须通过共享的 `ProductionIdentity` 固定 Team 配置、真实运行进程的 Security requirement、外层 app 严格签名/全部架构/嵌套代码校验、固定文件布局和非符号链接检查，以及 helper 自身签名 requirement。Info.plist 的自报身份不作为认证依据。任何门禁失败都返回具体 `blocked` 原因，且不会调用注册或注销 API。当前 Team 尚未配置，真实签名和系统注册均未验收。

`status` 始终只读查询固定服务，并单独报告当前身份和包的门禁结果。`service_state=enabled` 仅表示系统注册状态；它不会把 `production_ready` 或 `hardware_write_enabled` 变为 true。`requires_approval` 明确区分系统仍在等待用户批准。未知系统状态显示 `unavailable`。注册、注销返回的是请求结果和读回状态，不保证后台进程已经启动、停止或完成认证。

注销仍要求固定生产客户端的真实签名、app 结构及完全匹配的固定 daemon plist，但允许 helper 文件已经丢失。为清理这种残留服务，注销只校验当前 app 可执行代码签名，不校验 app 资源封印，也不要求嵌套 helper 存在。plist 缺失、被改写或成为符号链接时仍会阻止注销；不会退回任意标签的 `launchctl remove`。当前 helper 没有启用硬件写入；未来若允许控制硬件，必须先增加经认证的停止控制与恢复系统自动控制协议，取得恢复确认之后才允许注销。当前实现不能被当作硬件恢复机制。

运行 `python3 helper/scripts/service_manager_probe.py` 可在普通用户下编译严格告警的 macOS 11 目标并执行否定测试：真实配置缺 Team、虚构 Team requirement 拒绝当前未正式签名客户端，以及模拟 macOS 12 在调用任何服务 API 前直接返回 unsupported。所有 ServiceManagement 调用均替换为计数 stub；验证 enabled/approval 等状态不会提升 readiness，两个 mutator 计数始终为零。测试还检查生产 object 没有静态 SMAppService 类依赖。测试构建的注入 API 不包含在生产编译中，且测试构建不会回退访问真实系统服务。

以上不代表已完成 Developer ID 签名、正式 app 安装、管理员批准、root daemon 运行、真实服务注册/注销或硬件验证。系统 API 语义参见 [Apple SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice)。
