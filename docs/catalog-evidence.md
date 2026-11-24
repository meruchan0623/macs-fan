# ARM 传感器目录的证据和边界

`src-tauri/src/catalog.rs` 是纯元数据模块，不读取或写入硬件。描述存在不能证明键存在或测量有效；采样层必须核对真实枚举、读取成功和质量条件。中文标签为本应用自有简洁表述，应用未嵌入原厂 XML、英文文案、解析器或二进制。

本 API **仅提供 ARM SMC 目录**，调用者负责架构路由，不能用于宣称 Intel 目录已实现。统一的 `provisional_mapped` 表示资源有声明，未匹配项保持 `raw` 与原始键名，不代表物理部件校准或跨机型实测。

## 静态来源

证据均在交付根 `outputs/` 下，应用不在运行时加载。目标为主程序 arm64 切片 SHA-256 `a0bc2a0fba48ebfaa2b3edd385bd2050ef8b05d599ed363c39f772998ad2fc3d`。提取过程见 `outputs/deep-analysis.md`，新恢复契约见 `outputs/provider-analysis.md`，完整 REA Evidence/限制见 `outputs/evidence/provider-deep/evidence-index.json`。

| 证据 | SHA-256 / 地址 | 用途 |
|---|---|---|
| `evidence/deep/resources/idSwapRules.xml` | `8c7ab1f593e8f112485aa397b8656d5895b9ec07ea0b3e22965b49fef18c4fe5` | 第3–48行全部46个完整机型映射 |
| `evidence/deep/resources/smcSensorsArm.xml` | `b46187561e556b8e749fee230cc01956050de9ff352d30a7aac55c37b5986108` | 全19块、232条键事实、24个组声明 |
| `evidence/provider-deep/pseudocode/model-match.c` | `0x10005146c` | all、分号 OR、同家族范围下限 |
| `evidence/provider-deep/pseudocode/xml-load-once.c` | `0x10003cd28` | 所有匹配块按文档顺序载入 |
| `evidence/provider-deep/pseudocode/xml-load-matched.c` | `0x10003bf28` | 后项覆盖、exclude属性存在即排除、分组 |
| `evidence/provider-deep/pseudocode/supported-sensor-constructor.c` | `0x10003b9e8` | 第一个非空标签为主名，后续为同源别名 |
| `evidence/provider-deep/pseudocode/smc-load-available.c` | `0x100046620` | 别名引用原对象，未生成第二次物理测量 |
| `evidence/provider-deep/pseudocode/average-groups.c` | `0x100041558` | 唯一物理成员、hideSensors不删除读数 |

每个描述的 `evidence` 保存资源、行号与完整 models 范围。`normalization_evidence` 独立提供机型归一化证据。全部结论来自静态分析；本轮没有运行原程序、原 helper 或进行硬件写入。

## 匹配与顺序

归一化只接受46条完整字符串映射，区分大小写、不 trim、不推断未知 MacNN。`all` 属性整体精确为小写 all 时无条件匹配；所以未知 ARM 机型也可取得 TA0P、TB0T、Th1H、TPMP、TW0P 五个明确描述，其他未匹配键仍 raw。`all;…` 没有该特殊含义。

分号列表按 OR；范围限定同一家族、包含下限。原 `parseModelAndVersion @0x100050d68` 删除非数字并转 ushort，数字版本来自 major/minor 的串联，例如21,2→212；这里保留正规资源的比较含义，**不是 major/minor 元组排序**。新实现仅接受 `已识别家族 + 正规十进制major,minor + 末尾单个加号`，数字不允许冗余前导零，转换必须不溢出 u16。不放大原函数对任意 contains('+') 异常文本的宽松行为；空白、额外符号、未知家族、溢出均拒绝范围匹配。

所有匹配块继续处理，后出现的同 key 先移除旧记录再加入新记录，标签/别名/分组/隐藏元数据一起覆盖。排除同样清掉成员关系，空组不产出均值。ARM 当前没有 exclude 声明；通用规则解析器的排除分支由独立构造测试验证，不声称已经导入 Intel XML。

完整事实对账为 **195条精确块 + 37条非精确块 = 232条**。37包含all的5条和范围的32条；4个范围块中有5个带加号的 token（第一块含两个家族）。24是源组声明数，不是任一单机型的组数。

| 源行 | models | 键事实 / 组声明 |
|---|---|---|
| 3–9 | `all` | 5 / 0 |
| 10–13 | `MacBookPro17,1+;MacBookAir10,1+` | 2 / 0 |
| 14–17 | `iMac21,1+` | 2 / 0 |
| 18–20 | `Macmini9,1` | 1 / 0 |
| 21–58 | `MacBookPro18,1;MacBookPro18,2;MacBookPro18,3;MacBookPro18,4;MacStudio1,1;MacStudio1,2` | 32 / 2 |
| 59–64 | `MacStudio1,1+` | 4 / 0 |
| 65–87 | `MacStudio2,1;MacStudio2,2` | 17 / 2 |
| 88–101 | `MacBookPro17,2;MacBookAir11,1;MacBookAir11,2` | 10 / 1 |
| 102–121 | `MacBookPro19,1;MacBookPro19,2;MacBookPro19,3;MacBookPro19,4;Macmini10,1;Macmini10,2` | 16 / 1 |
| 122–124 | `Macmini10,1` | 1 / 0 |
| 125–144 | `MacBookPro20,1;iMac22,1;iMac22,2;MacBookAir12,1;MacBookAir12,2` | 14 / 2 |
| 145–166 | `MacBookPro20,2;MacBookPro20,5` | 16 / 2 |
| 167–190 | `MacBookPro20,3;MacBookPro20,4;MacBookPro20,6;MacBookPro20,7` | 18 / 2 |
| 191–209 | `Macmini11,1;MacBookPro21,1;iMac23,1;iMac23,2;MacBookAir13,1;MacBookAir13,2` | 13 / 2 |
| 210–230 | `MacStudio3,1;Macmini11,2;MacBookPro21,2;MacBookPro21,3;MacBookPro21,4;MacBookPro21,5` | 15 / 2 |
| 231–245 | `MacBookPro22,1;MacBookAir14,1;MacBookAir14,2` | 9 / 2 |
| 246–275 | `MacBookPro22,2+` | 24 / 2 |
| 276–305 | `MacStudio3,2` | 24 / 2 |
| 306–320 | `MacBookNeo1,1` | 9 / 2 |

## 同源别名、均值与隐藏

`Descriptor.display_name` 是首个标签，`aliases: Vec<String>` 是同源标签。例如 Mac16,8 的 Tp01：主名 CPU性能核心1，别名 CPU性能核心7，共用 Tp01 的一个读数。之前“候选编号/选择规则未核实”的措辞已纠正。不生成 Tp01-0 硬件键、不新增第二个读数、不重复计入均值。

均值继续使用现有应用 ID `avg:cpu`、`avg:gpu`；`member_ids` 是非空且唯一的四字符物理键，目录只声明成员，不计算温度。后项改组或排除后，旧组不再包含它。Macmini10,1 的额外 Tp0n 位于组外，不能因标签相同就移除 Tp0X 或擅自加入均值。

`hide_sensors` 在物理描述上表示源组请求隐藏该成员，在均值描述上表示源组请求隐藏成员；不代表该描述缺失或无效。GPU Core Average 的6个成员隐藏属性为true，其余已声明ARM组为false。UI概览可采用自己的展示策略。目录不处理 `add_no_fail` 的硬件装载效果；它不能替代真实枚举或有效性检查。

原名称过滤还会按 CPU 信息限制 Performance/Super Core 标签，字段上限来源未完全恢复；本模块没有 CPU 数量输入，保存未过滤标签并在别名说明保留此边界，不把标签数当作硬件核心数。缺失或失败成员不能补0冒充有效均值；质量策略由采样层决定。

Mac16,8→MacBookPro21,2（映射行36），命中all、MBP17,1+触控板范围与含21,2的精确块，不命中MBP22,2+。新增 Ts0P/触控板（行11）、Ts1P/触控板执行器（行12）；CPU均值8个唯一源、GPU均值5个唯一源。含可选参考键在内共26个描述，不是实机检测数量。

TCDX、TCMb 的独立来源仍为 `evidence/deep/pseudocode/low-value-correction.c` 行35、41；只对有精确ARM块的模型附加参考描述，范围-only模型不自动获得。`model_scope=not_model_specific; gated_by_exact_arm_catalogue` 明示该保守应用策略。具体物理位置与芯片结温未证实，两键不进入核心均值，也不对低温读数补值。

## 接口与验证

原有 `normalize_model`、`normalization_evidence`、`has_exact_catalogue`、`catalogue`、`classify` 和既有字段保持；新增 `aliases`、`hide_sensors`。架构入口新增 `catalogue_for_architecture`，仅 `arm64`/`aarch64` 走本目录，Intel分支独立见 `docs/intel-catalog-evidence.md`。crate内 `keys_match_for_architecture` 仅供元数据查找，保留真实读取键。`has_exact_catalogue` 仍只回答精确块是否存在，不能作为all/范围的可用性门槛。group稳定为CPU/GPU/Battery/Other，classification可调用 `as_str()`。

独立验证命令（不构建 Tauri，不访问硬件）：

```sh
rustfmt --edition 2021 outputs/macs-fan/src-tauri/src/catalog.rs
cat > /tmp/mfc-catalog-harness.rs <<EOF
#[path = "$(pwd)/outputs/macs-fan/src-tauri/src/catalog.rs"] mod catalog;
#[path = "$(pwd)/outputs/macs-fan/src-tauri/src/intel_catalog.rs"] mod intel_catalog;
EOF
rustc --edition 2021 --test /tmp/mfc-catalog-harness.rs -o /tmp/mfc-both-catalog-tests
/tmp/mfc-both-catalog-tests
```

12项测试通过，覆盖46映射、未知型号与all、5个范围下限/跨家族、legacy数字比较、异常/溢出fail-closed、别名同源、覆盖/排除清组、hide元数据、唯一成员、Mac16,8新增键、195+37事实与24组、参考键隔离。另以独立Python逐块对账源XML与Rust的顺序范围、232条key/行号、29个别名数量、24组的源行/唯一成员/隐藏声明，全部一致。

Intel目录已在独立模块实现，见 `docs/intel-catalog-evidence.md`。本ARM目录仍未实现IOKit目录及采样、风扇位置规则、原CPU计数过滤的硬件输入，以及所有控制动作。静态目录测试不代表各型号硬件检测已经通过。

## 本轮核对的架构比较器

ARM `isSameSensor @0x1000458dc` 在已保存 `outputs/evidence/native/arm64-disassembly.txt` 中直接跳到QString operator== stub `0x1001eb764`，因此严格大小写。`SensorsFromXML::sensorByKey @0x10003d140..158` 与remove路径都调用保存的比较器。ARM Neo的common TW0P和精确块TW0p保留两条静态声明；没有把Intel末字符等价规则套到ARM。实际读取仍使用枚举原key（`evidence/provider-deep/pseudocode/smc-load-available.c` 的pQVar18），目录匹配只提供标签和分组。

x86同名比较器是不同实现：前三字符精确，仅第4字符大小写等价；位置、资源绑定与lookup证据见 `docs/intel-catalog-evidence.md`。独立测试新增架构隔离及Neo回归，当前双模块共20项通过。
