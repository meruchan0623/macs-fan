# 宿主候选温度采样

成功构造只读 SMC 宿主时创建一个独立温度 worker；既有 `Backend::SmcReadonly` 不变。温度连接与 actuator 连接分离，不获取 actuator mutex。身份检测、`TemperatureReader::open` 和所有温度读取都在 worker 中执行；跨线程的是初始化工厂，reader 本身没有 `Send` 约束或新增 `unsafe impl Send`。

固定 `/usr/sbin/sysctl -n hw.model` 只在 worker 初始化执行，无 shell、PATH 查询或客户端参数；检查退出状态、UTF-8、长度及精确机型值，再结合本机架构选择候选映射。当前仅接受 `Mac16,8` + `aarch64`，其余机型或架构不开温度连接。没有泛用 T 键扫描或回退猜测。

| 内部 sensor ID | SMC 键 |
| --- | --- |
| `smc:TAOL` | `TAOL` |
| `smc:TB0T` | `TB0T` |
| `smc:TCMb` | `TCMb` |
| `smc:Tp01` | `Tp01` |

映射依据是既存 `outputs/evidence/temperature-reader-0.12/probe.log`：这四键在 Mac16,8/aarch64 上成功进行过只读协议解码。这里只给原始键身份，不声明物理位置或校准；没有重跑探针。映射按内部 ID 的规范排序，与 worker 返回值顺序一致，测试用四个不同值验证对应关系。

客户端不能指定读取键、时间、质量、身份或准入。四个来源始终未准入，成功批次只标 `Unverified`；`supports` 必须同时满足来源已准入且 worker 仍活跃，因此这些候选即使已存在也不能开始 sensor plan。只有显式 fake 测试来源可标 `Verified`。混合 fixed/sensor 计划若含未准入候选，会在任何 fan 应用前整体拒绝。

每轮最多四次读取，候选周期为一秒；没有并发读任务或批次队列。每次读取核对返回身份和键、排除 suspect/正负零、非有限值及 -40…150°C 之外的值。候选范围与 adapter 解码范围一致；不把 control policy 的 -20…127°C 范围当作候选解码范围。任何一项失败生成完整 `Failed` 批次并终止 worker，不补零、不复用上一值、不重启。

样本时间取每轮读取开始时的宿主 monotonic epoch；序号仅递增，毫秒未前进就等待，不制造未来时间。单槽邮箱覆盖旧批次，宿主取批次时不等待 I/O。初始化前、身份检测返回后开启连接前及初始化返回后均检查取消与250ms预算；初始化或整轮读取返回时若已经超过 250ms，结果丢弃并终止 worker，不发布迟到成功。这个预算只能在同步操作返回后判定，不能中止挂住的 sysctl 或 IOKit。挂住的 worker 不阻塞 watchdog、stop 或宿主 Drop，也不会产生新样本；真实候选本来就没有控制资格。既有已准入 fake 控制来源的过期样本仍由 runtime 的样本超时退出。

停止/Drop 设置 stop 标记并清空单槽，晚回不可留下批次。睡眠、意外唤醒、关机同时停止 sampler 并清除宿主缓存；唤醒不重建原 sampler 或恢复计划，重新采样需要新建宿主。detached worker 如果仍堵在同步调用中可能暂时存活；这不代表可用来源。异常退出也不能把残留成功批次当作活跃缓存。

当前闭合 wire snapshot/inspection 没有候选字段，本轮未扩展它，也没有添加 GUI 诊断。最低观察产物是宿主内部 `Batch`、私有 factory/read seam 及零硬件单元测试。只读宿主构造接线已经实现；后续主代理36项非特权SMC只读回归启动了这个只读宿主，但没有导出内部候选批次，不能据此声称候选持续实机采样已完整验收。没有注册系统服务、root运行或真实硬件写入；只读载体保持`dry_run=true`与`hardware_write_enabled=false`。0.16正式安装版仍在身份门禁处阻止生产宿主启动。

验证使用 `cargo fmt --manifest-path helper/Cargo.toml --check`、`CARGO_TARGET_DIR=/tmp/macs-fan-candidate16-target cargo test --offline --manifest-path helper/Cargo.toml` 与相同 target 下的 `cargo clippy --offline --manifest-path helper/Cargo.toml --all-targets -- -D warnings`。测试覆盖未准入候选已存在也拒绝计划、固定映射、非 Send reader 的 worker 内创建、未知身份不打开连接、初始化失败/迟到、逐读身份/键变化、suspect/零/范围边界、控制线程不等初始化、晚回及 Drop 不发布，以及睡眠/唤醒不重启。
