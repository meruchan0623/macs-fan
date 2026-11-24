# AppleSMC 受限执行适配层

这是独立 Rust crate，直接实现 `macs_fan_control_core::Actuator`。当前已实现真实读取、command 6 写入封包与受限写序列，**尚未验证任何机型的硬件控制和自动恢复**。本阶段只对真实硬件运行只读 probe；测试中的写入全部发生在内存 mock transport。

## 接口与边界

```rust
use macs_fan_control_core::{Actuator, FanId};
use macs_fan_smc_adapter::SmcAdapter;

# fn example() -> Result<(), Box<dyn std::error::Error>> {
let mut adapter = SmcAdapter::open_readonly()?;
let fans = adapter.enumerate().map_err(|e| format!("{e:?}"))?;
for fan in fans {
    let _state = adapter.read_state(fan);
    let _capabilities = adapter.capabilities(fan);
}
println!("{}", serde_json::to_string_pretty(&adapter.probe())?);
# Ok(())
# }
```

- `open_readonly()` 永久抑制该实例的 profile admission，没有提升权限的方法。独立 probe 使用此构造函数。
- `open()` 仅依据真实 `sysctl hw.model` / 编译架构，查询内部、静态、经代码审核的 production profile。该表目前无条目；所有写调用返回具体拒绝原因。空表只是当前验证状态，不代表协议功能完成。
- 唯一公开写接口来自 Actuator：`write_target(FanId, rpm)`、`write_mode(FanId, WriteMode)`。没有原始键、任意字节、JSON profile、`validated` 入参、环境变量或 feature 开关。
- `Transport`、固定键枚举、`Profile` 和操作绑定的 `Admission` 均私有。真正的 command 6 调用还验证 Admission 来源地址属于静态 production registry。测试中手工创建的 mock profile 无法通过此检查。
- `capabilities()` 由真实读取生成。发现 fpe2/float 与范围不使 `validated`、model support 或模式控制变成 verified；未验证时 staging=false。未验证的 step 是编码层的保守候选值，不是已验证的固件步进权限。
- `last_error()` 保留 ActuatorError 粗分类之外的具体 IOKit 原码、SMC 结果码、键、元信息漂移和读回错配。SMC 0x84 单独表示 MissingKey；其他非0结果不做缺键回退。

## 已实现的协议

内部键仅为 FNum、FS!（末尾空格）、Ftst，以及有界编号0–15的 Ac/Mn/Mx/Tg/Md/md。实际RPM是 Ac，命令目标是 Tg，两者不混用。

IOKit ABI 为显式填充、初始化为0的 `repr(C)` 80字节结构，selector=2；读 key-info command=9、读值 command=5、写值 command=6。每次操作重新读取相关元信息，不从先前成功值合成实时状态。SMC连接和服务均释放关闭，不访问原厂 helper。

目标编码只接受元信息确定的 fpe2/2（RPM×4，两字节大端）或 flt/4（可精确表示的有限float32，小端）。不因长度相同就容许任意类型；不静默截断分数。profile分别固定 Mn、Mx、Tg和模式键的类型、尺寸、attributes，避免合法的每键属性不同被误判，又确保之后漂移会被拒绝。

每次受限写前核对真实身份、FNum、全部 profile 风扇范围和关键元信息、模式0/1语义。未知原始3保持 Unknown；不会按某个工具的启发式猜为自动。

- 目标：确认自动模式 → 写 Tg → 读回 Tg与模式，要求目标精确相等且仍为自动。这是 `control-core` 的 target-staging 前置协议；它是否在具体固件上安全必须独立验收。
- FS!：读取完整16位掩码 → 仅更改目标 fan 位 → 写回 → 读回完整掩码。保留所有其他位，包括枚举风扇之外的位。
- Md/md：只写 profile 指定大小写模式键 → 读取确认 → profile要求时读取全部模式、计算 `any(manual)`、写 Ftst → 读回 Ftst → 确认目标模式。手动启用还确认 Tg未被模式写入意外改变；恢复自动后不要求固件保留旧手动目标。
- 已验证的Ftst路线在每次状态读取时同时检查全部direct模式与全局gate是否一致；仅Md=0不足以确认自动恢复。gate不一致时返回读回错误，核心引擎不能据此提前结束恢复。自动恢复允许修复有效0/1值之间的不一致，仍保留身份、范围、元信息和模式语义预检；手动启用不绕过一致性检查。
- 命令返回成功但读回不符仍是失败。不会在适配层进行无界重试或谎报恢复成功。重试/租约/让出外部控制的职责由 `control-core` 和独立 helper 持有。

只读状态探测优先观察 FS!，缺键时观察大写Md再小写md；同时可见的FS!/direct结果冲突时返回Unknown。这是本模块保守状态解释，不声称固件控制权已验证，也不冒充原程序的完整读取规则。写路径只使用被profile固定的路由，不进行失败后尝试其他写键。

## 实机只读结果

2026-11-22：Mac16,8 / aarch64。FNum=2，FS!缺失，Ftst=ui8/1且原始0；F0Md/F1Md=ui8/1且原始0；F0md/F1md缺失。两个风扇的Mn/Mx/Tg为flt/4，min=2317、max=7826。观察到attributes分别为Mn132、Mx133、Tg212、Md/Ftst208；不应要求这些属性彼此相等。

Mac16,8归一化到MacBookPro21,2，落在所分析原程序 `202≤版本<216` 的 not-working 谓词内。此事实不会被描述为固件永远不可控，但在缺少独立恢复证据时明确拒绝写入。完整 probe 输出存于 `evidence/readonly-probe.json`，其中 hardwareProfileVerified=false、hardwareWriteAttempts=0。

## 验证

在本目录执行（构建缓存位于/tmp）：

```sh
CARGO_TARGET_DIR=/tmp/macs-fan-smc-adapter-target cargo test --offline
CARGO_TARGET_DIR=/tmp/macs-fan-smc-adapter-target cargo clippy --offline --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/macs-fan-smc-adapter-target cargo run --offline --bin readonly-probe
```

测试验证精确编码、逐fan大小写完整写序列、Ftst any-manual、FS!保留他fan/未枚举位、元信息与范围/身份漂移拒绝、写后目标/模式/gate错配、错误码传播、无production profile时写拒绝，以及mock token不能授权真实传输。ABI/command6纯封包测试不调用设备。

针对Ftst恢复缺陷的回归还使用真正的`control-core::Engine`驱动适配器和mock transport：租约释放时Md写0生效但Ftst写0不生效，不得变成AutomaticConfirmed；在251ms重试时允许修复gate并确认自动；连续失效则三次恢复后保持exhausted，后续Tick不再写入。大写Md与小写md两种路线均覆盖，并验证另一fan仍为手动时gate=1合法。

## 启用真实控制前的剩余验收

必须以每个精确真实机型、架构及系统/固件范围保存审查记录，包含原始元信息、允许风扇数、路由/大小写、Ftst语义、逐fan范围/步进/零转许可、所有相关关键字节及恢复证据SHA-256；还需定义系统升级后失效策略。单独读到键、单次成功手动写、厂商机型表或mock测试均不够。

硬件验收至少包括：

1. 自动状态写目标不接管固件控制；手动启用与目标读回一致；上下边界、量化步进、零转/禁零语义；他fan模式与保留位不受影响。
2. 正常释放、租约到期、断连、客户端崩溃、helper进程崩溃/重启、睡眠/唤醒、SMC读取与写入失败、已生效但超时、metadata变化和热状态变化后的自动恢复。逐项观察恢复读回和持续散热行为，不能仅用API返回值判断。
3. 存在其他风扇控制器时的控制权冲突、让出策略和恢复不抢占；Ftst any-manual对另一fan手动会话的影响。
4. 独立签名的 helper、逐请求认证、可信串行引擎、独立周期Tick/看门狗和生命周期事件。适配层自己不认证客户端，不能替代这些边界。
5. **硬超时仍未实现**：同步 `IOConnectCallStructMethod` 不能在Rust调用中安全取消。当前只在返回后将超过250ms的完成标记Timeout，不能声称调用在250ms内退出。启用前需经进程隔离与独立恢复机制验收内核调用卡住时的行为；线程超时丢弃返回不等于停止硬件操作。

完成上述实际验收后，才可经代码审核向私有registry加入profile。RecoveryEvidence中的布尔项只记录审查状态，本身不是证明；必须审核对应的独立证据文件。测试profile不进入生产注册表。

## 来源

实现为独立Rust代码，没有复制原厂实现或复用原厂特权组件。协议与键语义依据本项目的 `outputs/helper-analysis.md`，及 `evidence/helper-deep/pseudocode/smc-write-key.c`、`main-set-fan-modes.asm`、`main-set-rpm-by-index.asm`、`main-get-fan-modes.asm`。Mac16,8边界来自 `evidence/deep/pseudocode/model-not-working.c` 与提取的机型别名事实。ABI另与公开的 [Stats SMC源码](https://github.com/exelban/stats/blob/master/SMC/smc.swift)（MIT，Serhiy Mytrovtsiy）交叉核对；未引入其实现。
