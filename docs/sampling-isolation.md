# 采样进程隔离

`sampler::Manager::request_snapshot()` 是父进程唯一采样入口，返回 `Result<Snapshot, String>`。Tauri 状态使用 `Arc<Mutex<Manager>>`，在 `spawn_blocking` 中加锁并调用它；不要在 UI 进程创建 `Monitor`。`main` 必须先识别固定参数 `--sampler-worker`，调用 `sampler::run_worker()`，错误仅写 stderr，然后退出，不进入 Tauri 初始化。Snapshot、Fan、Sensor 需同时派生 Serialize / Deserialize。

父进程通过 `current_exe()` 直接启动同一程序，固定传入 `--sampler-worker`，不调用 shell。stdin 只发送递增的非零十进制请求序号（最多20字节），以 LF 结尾；stdout 只允许单行 `{request_id,snapshot}` JSON，以 LF 结尾。父进程校验响应序号严格对应当前请求；重复、过期或错序响应使 worker 失效。序号溢出后关闭该 Manager；worker 拒绝非递增请求。stderr 继承父进程用于诊断，不混入 JSON。每个 worker 保留一个 Monitor，因此成功请求之间保留目录和元数据缓存。

父进程使用独立读写线程处理管道，响应队列容量为 1，JSON 行最多 4MiB（不计 LF）；缺少换行、坏 JSON、超长行、EOF、管道错误都使本 worker 失效。读取实现按缓冲块检查长度，不先无限制地分配整个输入。响应等待以请求开始时间计算 6 秒截止，启动耗时也计入预算；写请求不会占住请求线程。

错误或超时后，独立回收线程持有 Child、管道线程和关闭请求队列的责任，依次 kill、wait、join。请求线程立即返回错误。只有确认旧进程及管道线程退出，下次请求才允许创建新 worker。回收未完成时返回“仍在回收”，不创建额外进程；回收失败/回收线程失联则关闭自动重建。`wait` 出错时回收线程保留 Child、报告 stderr 并继续回收，不丢弃可能存活的子进程。Manager Drop 将现有 worker 交给同一回收流程；应用退出关闭 stdin。

worker 有独立 stdin 监测线程：父进程正常退出或崩溃导致 EOF 时，调用进程退出，即便主采样线程正在等待驱动。协议输入非法或请求排队异常也会退出。父进程不需要发额外终止协议。

## 能保证的边界

UI 进程不再执行 SMC 驱动调用。普通用户态阻塞和可终止的驱动等待会触发超时、杀死并回收旧 worker，后续请求能重新采样。

**kill、进程退出和 wait 都不能保证终止不可中断的内核工作。** 若内核使旧进程无法回收，该 Manager 保持暂停重建，不能宣称已恢复，也不会不断启动更多 worker。父进程崩溃时 stdin EOF 会请求 worker 退出，但相同内核限制仍然存在。这个约束跨应用重启无法仅靠当前进程内状态排除，需要操作系统恢复或重启。

6 秒是用户态响应等待预算；操作系统执行进程创建、线程创建或调度本身若发生无限停顿，普通应用无法给出硬实时保证。进程/管道线程初始化失败会关闭此 Manager 的自动重建，避免尚未确认回收的进程被替换。线程资源完全耗尽导致无法启动回收线程也是系统资源故障边界，不能据此声称已安全回收。

## 验证

执行 `cargo test --manifest-path outputs/macs-fan/src-tauri/Cargo.toml sampler::tests`。测试通过 python3 可执行程序和固定脚本参数建立真实管道，不运行 shell，不访问 SMC：

- 成功响应、同一 worker 复用；
- 重复旧响应不能冒充下一次请求的新采样；
- 坏 JSON、超过 4MiB、EOF、缺少 LF 后确认回收及恢复；
- 250ms 测试截止、阻塞 worker 的终止与恢复；
- 回收仍在进行/失败时拒绝重建；
- Manager Drop 后确认子进程 PID 消失；
- 回收完成信号在进程和管道线程均退出后才发出。

另以临时 Rust harness 引入原始 sampler.rs，将 Monitor 换成睡眠 60 秒的 stub：stdin EOF 在空闲和采样阻塞两种情况下都以状态 0 退出，实测分别约 9ms、2ms。此检查不模拟不可中断的内核状态，也不代表真实硬件驱动通过验收。

4MiB响应上限覆盖4096候选键及分类/来源元数据和极端错误诊断；最大枚举规模的实际Rust JSON序列化测试验证该边界。所有行仍先按缓冲块限长，超限不会无限扩展内存。
