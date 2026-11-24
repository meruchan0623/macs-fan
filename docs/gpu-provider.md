# Intel registry GPU 温度

`src-tauri/src/gpu.rs` 是依据已恢复接口契约独立实现的只读 adapter，没有复制原二进制实现。唯一温度来源为 IOService 平面的直接属性 `PerformanceStatistics` → CFDictionary → `Temperature(C)` CFNumber。没有 SMC 猜测配对、Metal 设备推断、硬件控制、授权请求或用户客户端写命令。调用者必须使用已有隔离采样子进程；同步 IOKit 调用没有可靠的进程内取消机制。

## 证据与差异

恢复依据为 `outputs/extra-providers-analysis.md`，原程序未滑移地址：Intel `GPU::ProviderMac::discoverGPUs @0x100067f70`、`getCoreTemperature @0x1000693f0`；arm64 dedicated GPU `getCoreTemperature @0x10006ad7c` 为 zero stub。原证据位于 `outputs/evidence/extra-providers/` 的 `x86_64-100067da0-1000696a0.asm`、`x86_64-gpu-sensor-load.asm`、`cfstrings.json`、`gpu-temperature.json`；完整证据索引为该目录的 `evidence-index.json`。

新实现只接受 IOPCIDevice `class-code` 为 CFData，取前四字节 little-endian，匹配 `0x30000 / 0x38000 / 0x30200`。遍历 display-class 节点及其子树；每个节点直接 CreateCFProperty 读取，不使用递归 SearchCFProperty，以便 `source_id = gpu:registry:{16hex}` 始终指向实际属性 owner。同一 owner 多次遇到只保留一次。registry ID 只在对应 IORegistry 对象生命周期内有效，不是跨重启身份，也不能证明多个节点对应不同物理探头。`name` 是 owner 的 registry 名，未声称为显卡 model。

原实现可能把缺失值转成 0、保持旧值、锁存 SMC 回退；新实现每次重建 Batch，没有跨样本缓存。统计对象非字典、温度非 CFNumber 或转换失败为 invalid；温度缺失为 unavailable；NaN、无穷及 -40…150°C 以外值为 invalid 且数值为空；真实原始 0 保留为 suspect；范围内其它数值为 valid。valid 只表示候选数值检查通过，不表示传感器校准或硬件覆盖率得到确认。CFBoolean 和字符串数值不会被强制解析。

Apple Silicon `sample()` 直接返回 `supported=false / available=false` 与 `not_applicable` 诊断，绝不调用 native 枚举、读取 zero stub 或创建 0°C 温度。macOS native FFI 模块仍在 ARM 构建中编译，避免 Intel 实现完全绕过编译检查。

## 输出与资源边界

- `supported` 表示 Intel macOS 路径；`available` 只表示 PCI 枚举成功（SDK允许成功且无匹配时iterator为空），不能被解释为读到了温度。
- `service_count` 为本次检查的全部 PCI service 数，含非 display；无温度属性时 readings 为空并提供诊断。调用者依据 readings 与 quality 生成状态。
- PCI 服务最多256、所有 display 子树共享2048节点预算、根深度0、最大深度16（含根共17层）、输出最多64条。达到数量/深度限制设置 truncated。诊断最多32条、每条最多256字符；registry name 使用128字节buffer，字符串必须存在终止符。
- Get/Create 返回对象分别由 IOObjectRelease / CFRelease RAII 释放；字典内 CFNumber 是 borrowed，不能额外释放。Matching 字典交给 GetMatchingServices 消费（含失败）。空指针、类型和数值转换均检查。
- 失败不复用此前温度。PCI枚举失败返回本次 unavailable batch；存在统计属性但温度缺失/无效返回本次独立 reading。热插拔导致 iterator 无效会标记 truncated 并诊断。节点不提供统计属性时不为每个子节点创建 unavailable 行。

## 新验证与限制

新实现证据路径为 `outputs/macs-fan/src-tauri/src/gpu.rs` 的纯逻辑测试与本文件。2026-11-22 本机 ARM macOS 执行：

```sh
rustfmt --edition 2021 outputs/macs-fan/src-tauri/src/gpu.rs
rustc --edition 2021 --test outputs/macs-fan/src-tauri/src/gpu.rs -o /tmp/macs-fan-gpu-tests
/tmp/macs-fan-gpu-tests
```

6个测试通过（含真实CoreFoundation空句柄回归），覆盖 class-code 大小端及短数据、有限值/范围/0/缺失、owner 去重、下一次失败不继承成功温度、输出与诊断上限、ARM 不枚举。ARM 编译同时检查 native FFI 声明和资源管理代码的类型，但没有在 ARM 执行 Intel 枚举。没有 Intel 实机；未确认任何 Intel/AMD/NVIDIA 驱动实际属性覆盖、沙盒权限、热插拔、睡眠唤醒、具体系统版本或数值准确性。没有把静态恢复或纯逻辑通过当成 Intel 硬件验收。完整 crate 集成和隔离子进程 wire 测试由主集成工作验证。

独立对抗审查见上级报告 `outputs/review-gpu-0.13.md`。初始空指针包装器使用急切求值的then_some，曾触发CFRelease(NULL)；现改为惰性构造，并修正SDK允许的成功空枚举状态与mutable字典FFI签名。审查的14项测试使用fake IOKit和真实CoreFoundation，覆盖CF类型、分配失败、borrow/own释放、热插拔与全部枚举上限；它们不替代Intel实机验收。证据在 `outputs/evidence/review-gpu-0.13`。

监测集成不会将registry样本与SMC/HID猜测合并，也不会提升控制准入；overview在没有可用的主传感器时保留原始读数。窗口尺寸与表格布局保持不变。最大SMC/HID/GPU构造响应仍接受4MiB管道上限测试。
