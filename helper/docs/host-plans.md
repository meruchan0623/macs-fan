# 宿主计划与单账本（0.10）

固定计划现在走实际 NSXPC selector → Rust HostState → Coordinator → 同一 Engine。原先的 acquire/renew/set_target/release 也走这个 Coordinator 的受限 direct API，没有第二个共享执行器的引擎。当前生产宿主仍只读且发行 Team 未配置；本次成功执行只发生在临时非特权 Fake 服务，不代表真实风扇受控。

## 协议

仍使用 version=1、4096 字节闭合 JSON envelope；未知、重复字段与不正确类型拒绝。

```json
{"version":1,"command":{"op":"start_plan","entries":[{"fan":0,"strategy":{"type":"fixed","rpm":3000}}],"ttl_ms":5000,"seq":1}}
```

- `start_plan`: `entries, ttl_ms, seq`。1–16 个不重复 fan。strategy 为 `automatic` 或 `fixed`（rpm）；`sensor` 字段为 sensor_id/min_rpm/max_rpm/lower_temp/upper_temp，0.12由helper本地采样批次驱动；仅Fake宿主明确准入test:temperature，其他来源包括实际SMC候选均返回sensor_source_unavailable（尚无校准准入表）。整个计划转换完成前不启动任何 fan。
- `renew_plan`: `token, ttl_ms, seq`。只续客户端授权，不修改计划或冒充新温度样本。
- `stop_plan`: `token, seq`。停止该连接持有的计划并报告实际恢复结果。
- `plan_status`: 仅 op。报告最近状态、授权截止、核心租约截止；不返回 token。

start 成功仅向请求连接返回新的 32 字节 CSPRNG token（64 个小写十六进制字符）；后续变更必须 owner、token、严格递增 seq 同时匹配。owner 来自现有原生认证会话，不是请求字段。已发 token 有界记录防重用；最多65,536个计划授权，不无限增长。

`ttl_ms` 为客户端授权寿命，范围1–5000ms。Coordinator 内核心租约固定1000ms，两个截止时间分别命名。原生现有100ms timer独立于客户端请求执行 watchdog 和计划 step；仅当授权有效时续核心租约。每个请求先检查授权到期，返回前再次检查并重建状态；target/manual每次底层写入前还有独立授权期限门禁，Automatic恢复不受该门禁阻止。timer在步进前后也检查；迟到heartbeat不能重启已停止计划。连接丢失撤销其计划，其他连接断线不会停止此计划。首次请求不自动加载保存的预设。

## 生命周期与恢复

直接控制与计划互斥。无 session 时仍驱动直接租约的独立到期审计与恢复。计划启动拒绝账本中任何尚未确认恢复的旧租约；安全Stopped计划可以退休后开始新的直接操作，底层账本保留。sleep/shutdown即使没有计划也处理直接租约并锁定控制。wake不会自动恢复旧目标。

`automatic` 为观察项，不取得租约去改写外部控制；没有读回证据时为 unobserved。停止响应 ok 表示协议/执行流程完成，不等于所有风扇自动恢复，必须检查 fans 的 automatic_confirmed/recovery_pending/recovery_exhausted/yielded/unknown 等状态。

请求不接受样本、时间戳、质量、能力、硬件键或机型准入信息。SMC backend 保留 open_readonly 构造及包装层拒写；当前真实设备固定计划仍因 unverified_capabilities 被拒绝。

所有操作仍在原生串行队列上。100ms timer及操作后时钟检查无法抢占同步阻塞 IOKit，不能当作硬实时截止或进程死亡恢复。生产可写版本仍需要可信本地采样、GUI固定控制命令、正式双向身份、实机准入与独立故障恢复。

## 验证

helper新增7项单元测试覆盖授权身份/秘密/序号、不泄漏、原始命令互斥、过期、断线/睡眠、闭合schema、温度来源拒绝与Unavailable。真实非特权IPC新增12项：无客户端流量时跨越原核心租约期限仍由timer续租、计数证实授权到期由timer先撤销、晚到续租拒绝、错误owner和无关断线、停止读回等。真实只读测试逐个风扇拒绝start_plan，前后实例写尝试0。完整实际结果与独立审查见交付报告，不将Fake状态泛化为真实硬件结论。

实际调度观察：本机锁屏期间150ms租约后等待350ms，曾出现timer尚未处理、由查询入口才恢复的情形；证据timer-scheduling-0.10.json保留此失败。独立timer冒烟验证改为不发请求等待1250ms，再要求timer专属计数增长，查询自己恢复不能使检查通过。100ms是请求的调度周期，不是硬截止保证。

## 0.12 本地采样

独立worker维护有界单槽批次，和HostClock共享origin，以读取开始时刻标记样本；timer只非阻塞接收并且仅推进新sequence。没有新批次继续watchdog，不给缓存续时间；失败/死亡丢弃成功缓存、挂起由样本期限停止。请求状态和续授权都不生成样本、不推进策略。

sensor start先接收第一条本地样本并进入Preparing；两次独立样本通过前不写任何fan。客户端保存准备期token、保持独立续授权，并允许停止；准备期未读回风扇保持unobserved。实际Fake NSXPC验证Preparing→Running(3000 RPM)→Stopped自动读回，硬件写标记仍false。真实温度读取器仅返回candidate，不由目录映射或数值正常推导Verified。详情见local-sampling.md。
