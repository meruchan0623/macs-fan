# 只读 HID 温度 provider

实现文件是 `src-tauri/src/hid.rs`；无新增Cargo依赖，不包含Product/部件映射。`sample()`同步返回本次结果，由主采样流程在已有隔离worker内调用。没有权限申请、root、原厂helper、设备属性写入或硬件控制调用。

## 接口

```text
sample() -> Batch
Batch {
  available: bool,
  captured_at_ms: u64,
  service_count: usize,
  truncated: bool,
  readings: Vec<Reading>,
  diagnostics: Vec<String>
}
Reading {
  source_id: String,
  registry_id: u64,
  product: Option<String>,
  temperature_c: Option<f64>,
  quality: Quality,
  message: Option<String>
}
Quality = Valid | Suspect | Invalid | Unavailable
Quality::as_str() = valid | suspect | invalid | unavailable
```

`available=true`只表示provider成功创建客户端并读取到类型正确的服务数组，不保证有有效温度。空数组、全无效、全部身份缺失与provider不可用有不同诊断。时间戳是本次读取完成时间，不复用以前的读数。采样是顺序读取，不声称所有服务在同一硬件时刻测量。

## ABI与来源

本机Xcode SDK的 `IOKit.framework/Headers/hidsystem/IOHIDServiceClient.h` 声明 `IOHIDServiceClientCopyProperty` 返回调用方负责释放的CF对象；`GetRegistryID`则返回借用的 `CFTypeRef`。因此实现先检查CFNumber类型，再用SInt64读取ID，没有把返回指针误当整数ID。该接口也可由[Apple公开头文件](https://github.com/apple-oss-distributions/IOKitUser/blob/main/hidsystem.subproj/IOHIDServiceClient.h)核对。

本机CoreFoundation头文件用于核对CFIndex、CFTypeID、Boolean、CFRange、字典callbacks、CFStringGetBytes、CFNumberCreate/GetValue和Create/Copy所有权。64位macOS中分别采用isize、usize、u8，回调结构与CFRange显式repr(C)。GetRegistryID的借用对象不释放；服务对象由仍存活的服务数组持有；所有Create/Copy得到的客户端、字典、字符串、数值、数组、属性和事件均由RAII释放，包括返回类型错误的提前退出路径。

私有API声明与[Chromium 123的原始实现](https://chromium.googlesource.com/chromium/src/+/refs/tags/123.0.6285.0/components/power_metrics/m1_sensors_mac.mm)核对：Create、SetMatching、CopyEvent的64/32/64位参数及GetFloatValue的double返回。匹配常量和温度字段与[Chromium常量头](https://chromium.googlesource.com/chromium/src/+/c21e9f71d1f2e/components/power_metrics/m1_sensors_internal_types_mac.h)核对。采用UsagePage=0xff00、Usage=5、event type=15、float field=0xF0000。SetMatching的未公开返回语义不作臆测；与Chromium一样不据返回整数认定成功，而检查后续CopyServices的实际结果。

本项目的 `outputs/provider-analysis.md` 及 `evidence/provider-deep/pseudocode/iokit-load.c`、`iokit-update.c` 是原程序静态行为证据。当前实现独立编写，没有复制原厂或Chromium的实现；Apple公开头文件、Chromium BSD代码仅用于ABI和常量交叉核对。Apple当前公开仓库中的若干 `hid.subproj` 私有头为空文件，不能声称这些空文件提供了额外的私有ABI保证。

所有IOHID函数，包括类型ID和registry ID查询，都从固定系统路径IOKit通过 `dlopen`/`dlsym` 获得；没有把私有IOHID符号加入启动链接依赖。缺少任一必需符号、框架加载失败或客户端/服务数组为空/类型错误时，明确返回不可用及原因。仅稳定公开CoreFoundation符号直接链接。

## 身份、有效性与上限

- `source_id=hid:registry:{16位十六进制registry ID}`，稳定性以对应IORegistry服务生命周期为限，不保证跨重启仍相同。缺失/零/非CFNumber/无法精确读取的ID不生成伪稳定序号，省略该服务并设置truncated及诊断。
- 同一registry ID重复出现只保留一个源，记录诊断并设置truncated。同Product但不同registry ID全部保留，不按文本覆盖，也不据文本自动认定同一物理测点。
- Product按原文保留，不套用目录、前缀或组件推测。无Product或类型不符时保持None；超长/无法转换的属性不截断成看似可信的名称。CFStringGetBytes使用实际字节数，保留嵌入NUL及Unicode，避免CString结束符截短Product造成误映射。
- null事件是None/Unavailable；事件CF类型错误、非有限值、低于-40°C或高于150°C是None/Invalid；恰好0°C保留Some(0)/Suspect，并说明可能为占位。其他区间内有限值的Valid仅表示本次事件可解码且通过显示范围，不是部件映射或校准验证。
- 最多读取512个服务；Product最多256个UTF-16单元；UTF-8缓冲1024字节；最多64条诊断、每条最多512字符。超枚举上限、可信身份缺失、重复registry和不可保留的Product元信息都有诊断或truncated标记。无有效值不会静默归零。

该模块没有聚合逻辑。主程序可按精确Product映射，但必须保留registry ID区分同名服务，分组平均不得默默覆盖或重复计数。SMC与HID源也不能因为名称相似而合并成同一次测量。

## 验证与实机证据

独立验证无需构建Tauri，在项目目录执行：

```sh
rustfmt --edition 2021 --check src-tauri/src/hid.rs scripts/hid-probe.rs
rustc --edition 2021 -D warnings --test src-tauri/src/hid.rs -o /tmp/macs-fan-hid-tests
/tmp/macs-fan-hid-tests
rustc --edition 2021 -D warnings scripts/hid-probe.rs -o /tmp/macs-fan-hid-probe
/usr/bin/time -p /tmp/macs-fan-hid-probe
```

测试不访问硬件：覆盖null/0的区别、有限值/显示范围边界、同Product多源、缺失/重复registry ID、诊断有界，以及纯CF字符串的嵌入NUL/Unicode/长度上限和错误CF类型提前拒绝。

2026-11-22在Mac16,8/aarch64普通用户进程执行一次真实只读probe，墙钟约0.09秒；输出保存在本目录 `hid-readonly-probe.json`。结果available=true、serviceCount=77、readings=77、truncated=false；52条Valid、24条Invalid、1条Unavailable、0条Suspect。Invalid为PMU tdev等原始约-9200°C值，已保留异常说明而不显示正常温度；Unavailable为als-temp的null事件。24种Product对应多个不同registry ID，包括6个gas gauge battery服务，全部保留且source_id不重复。该样本不能证明其他机型/系统版本或隔离沙箱下同样可见。

## 剩余限制

私有API没有稳定兼容承诺，dlsym只能避免缺符号导致启动失败，不能证明未来系统保留同一ABI。同步客户端创建、枚举和CopyEvent无法在模块内安全硬取消；服务数上限只限制调用数量，不限制单个原生调用耗时。必须继续使用现有worker进程隔离和外部超时，不能把线程丢弃返回当成硬件调用已停止。

本轮没有为私有API错误分支进行真实权限故障注入，也没有跨机型、睡眠/唤醒、服务重枚举、长期性能或内存泄漏压力验收。当前worker进程可复用；每次sample仍创建客户端、枚举并释放对象，不跨采样缓存CF服务。若未来改为缓存客户端/服务，应另行评估其生命周期和失效刷新。模块不申请权限，遇到系统限制应报告不可用并由主程序保留原因。
