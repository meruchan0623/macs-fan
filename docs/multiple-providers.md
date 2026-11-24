# 多来源监测契约

0.5 仍是只读。SMC和HID均在现有sampler子进程中执行；主进程只接收有界JSON。没有添加控制命令或安装特权服务。

## 来源与身份

Snapshot新增providers数组，每项包含id/status/discovered/decoded/capturedAtMs。SMC的discovered/decoded计数仅指物理温度键，不含风扇或派生行；HID的discovered为服务数组数量，decoded为成功解码且可显示的读数（原始0可显示但仍标可疑）。这些计数不是物理核心数。

- SMC读数仍使用实际四字符key，目录只附元数据。ARM严格大小写；Intel仅允许最后字符大小写等价，依据两架构实际二进制差异。不能改变硬件读取key。
- HID身份为hid:registry:<registry ID>，稳定范围仅限该服务的注册生命周期。Product是可重复的标签，不是唯一ID。重复registry引用被去重并标记不完整；不同registry同Product均保留。
- 不合并SMC与HID的“同名部件”。不能依据相似标签声称测量了同一个物理位置。

HID Product目录是29项独立中文命名事实、5组声明，证据为evidence/deep/resources/IOKitSensors.xml，逐项Product与源行对账。精确匹配保留大小写、空白和嵌入NUL区别；未知Product保留原名，不自动猜CPU/GPU。目录的all范围不代表每台设备都存在这些服务。

## 聚合

ARM精确目录继续要求全部声明成员有效；Intel all-model组和HID组仅以当次实际发现的源构成成员集合。后者显示“已发现成员”，不声称完整物理核心覆盖；读取失败、可疑零值、非有限值、负值或超过150°C的成员不参与正常均值，组保持不可用。

Intel一个目录键匹配多个实际大小写源，或HID组有同Product不同registry时，保留独立读数但拒绝生成该组均值。没有选取最后一个、静默覆盖或双倍计权。模糊同名HID部件不挤入默认概览，仍可在部件/全部及诊断查看。每个派生行的members是实际源ID。

## 失败隔离与可见性

SMC访问失败不会丢弃独立有效HID读数：快照状态为partial，风扇数量保持未知。HID不可用时保留SMC实时值并记录该来源失败；空服务列表与调用失败不同。没有有效读数时仍unavailable。null事件是None/unavailable，原始0保留但assessment=suspect，越界值是None/invalid；这些状态不会被伪装为实时正常0°C。

所有采样仍使用整体开始时间判断过期，来源时间另保留，避免慢发现使较早读数显得更新。同步IOKit调用本身无法硬取消；6秒进程边界仍要求确认回收后再建worker，不能保证不可中断内核调用被取消。

上限：4096 SMC候选、16风扇、512 HID服务；HID Product最多256 UTF16单元、最多64条诊断且每条最多512字符。极限序列化测试联合覆盖这些边界：SMC按实际decode的负f32最大数值构造错误文本，HID按负f64最大数值；Product与诊断使用NUL压力测试JSON六字节转义膨胀。仍保持4MiB协议上限。

## 当前验证边界

Mac16,8实际HID77服务，52可显示、24越界、1空事件；约0.09秒。整合debug快照约0.196秒，SMC309物理温度键、HID77源以及3个派生行共389传感器记录。数值和质量随运行改变，不能把一次快照当长期稳定性证明。Intel只有静态资源/比较器证据与纯测试，尚无Intel实机；其他macOS版本、私有符号变化、更多HID服务类型和测量校准仍需验证。
