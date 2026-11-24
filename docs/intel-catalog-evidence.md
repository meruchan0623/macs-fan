# Intel SMC 描述目录的证据与边界

`src-tauri/src/intel_catalog.rs` 是独立 Intel 元数据目录。应用没有嵌入厂商XML、英文文案、解析器或原二进制；中文标签为本应用表述。键声明只是候选元数据，不证明硬件存在、读取成功、核心数或温度有效。本轮未运行原程序、原helper或硬件写入，没有打开新的REA会话。

## 架构入口与实现

`catalog::catalogue_for_architecture(architecture, model)` 对精确字符串 `arm64`/`aarch64` 走原ARM目录，`x86_64`/`x86`/`i386` 走Intel，其余返回空。`catalogue`/`classify` 原API仍保留ARM含义。机型归一化继续只使用已有46条完整映射；没有因架构字符串推测型号。`i386`/`x86`是新API接受的Intel路由别名，不意味着本次目标中存在32位可执行切片。

两个目录复用严格范围匹配、顺序覆盖/排除与分组解析，按来源分别保存 `evidence.file`。所有匹配块按源顺序处理，同key后项覆盖旧标签、别名和组，exclude存在则移除而不重新加入；均值只保留最后有效声明中的唯一成员，不留下已排除成员。范围同家族、包含下限，按major/minor串联的u16数字比较；异常格式、空白、冗余前导零和溢出fail-closed，与ARM的正规资源契约一致。不能把该目录称为原程序全量行为复刻。

`Descriptor` 的主标签、aliases、group、evidence、hide_sensors字段保留；Intel资源没有分号别名，两组hide均为false。均值ID继续为 **`avg:cpu`**。all的10个CPU核心键是通用候选，不能宣称每台机器具有10核心。采样层应把成员映射到实际发现且有效的物理键，不以声明数充当检测数量；若一个目录键同时对应多个真实大小写键，需保留各原始读数并标记均值来源不确定，不能猜测同物理源。目录自身不采样或计算数值。

## x86资源绑定的静态证明

原universal文件：`/Applications/Macs Fan Control.app/Contents/MacOS/Macs Fan Control`，SHA-256 `65146b1964ae6f5a37f87c00ea9bd3b1d9a51265f72f022fbb0c3458487da8b3`，lipo确认包含x86_64与arm64。普通系统工具的可复现命令：

```sh
/usr/bin/lipo '/Applications/Macs Fan Control.app/Contents/MacOS/Macs Fan Control' -thin x86_64 -output /tmp/mfc-intel-static
/usr/bin/otool -arch x86_64 -tvV '/Applications/Macs Fan Control.app/Contents/MacOS/Macs Fan Control' > /tmp/mfc-intel-disassembly.txt
/usr/bin/nm -arch x86_64 -n '/Applications/Macs Fan Control.app/Contents/MacOS/Macs Fan Control'
/usr/bin/shasum -a 256 /tmp/mfc-intel-static /tmp/mfc-intel-disassembly.txt
```

x86薄切片SHA-256 `2bca8a8509323d6ca94e3aaff06567c6f0275ac1adacabb4ccad71ee24c8c9aa`；本机otool完整输出SHA-256 `150b787e8f137792b16f936a4d9173dc21b33df8bece78c5d72c161274c0cf7b`。命令读取文件，不执行目标；以下是普通静态工具证据，不冒称新REA Evidence。

- 初始化 `0x100045727` 将 `:/Resources/xml/smcSensors.xml.gz` 地址载入rdi，`0x100045733`构造QString，`0x10004575e`写入 `TemperatureSensorSMC::_sensorsFromXML`。
- `loadAvailableSensors @0x100044b10` 在 `0x100044b2a` 将该对象传入，`0x100044b31`调用 `SensorsFromXML::loadOnce`。
- `loadOnce @0x10003acd0` 在 `0x10003ad0f` 读取资源，`0x10003ad35`解压，`0x10003ad52`交tinyxml2解析。`0x10003ae3f`匹配models，`0x10003ae5b`装载子节点，随后循环继续后面的modelMatch。
- `loadSensorsForMatchedModel @0x100039e80`：`0x100039f56`读exclude属性，`0x10003a0bd`先removeSensorByKey，`0x10003a0c2..d1`按属性指针是否为空决定跳过重加，没有把值解释成布尔0/1。

进一步按Mach-O的LC_SEGMENT_64做地址到文件偏移映射，静态遍历Qt v3资源表：`qInitResources_Resources @0x100076910` 在 `0x10007692e` 注册tree=`0x10021e170`、names=`0x10021e6b0`、data=`0x10021ef70`；本切片对应文件偏移分别为 `0x21e170/0x21e6b0/0x21ef70`。61节点中index58（树记录偏移 `0x21e66c`）实际路径 `/Resources/xml/smcSensors.xml.gz`，payload偏移 `0x252f7e`、1723字节、flags=0。

该压缩payload与已保存 `outputs/evidence/deep/resources/smcSensors.xml.gz` 逐字节相同，SHA-256 `6b292ef4e7bed6852c535d1a5296fc97d6a79b3b2bd4baed22f9088ecc87c1cd`；解压后也与 `smcSensors.xml` 逐字节相同，SHA-256 **`d01067ab9dfa74f0256a16c197f65a91ead94e961f6420dd218bad8ea5aaf642`**。因此资源文件名及实际内容均已绑定x86切片，未仅凭字符串存在推断装载。

## 键比较的架构差异

x86 `TemperatureSensorSMC::isSameSensor @0x1000442e0`：首参长度必须4；`0x100044302`将QString compare的CaseSensitivity设0，`0x100044304`比较完整字符串；相等后 `0x100044311..33b`取两者left(3)并严格比较。所以仅第4字符大小写等价，前三字符仍精确。新实现限定四个ASCII字节的真实SMC键，不扩大为全局大小写折叠。

比较器在x86初始化 `0x10004574b` 保存；`removeSensorByKey @0x10003aad0` 于 `0x10003abba/0x10003ac03` 调stored comparator，`sensorByKey @0x10003b080` 于 `0x10003b0b0..c2` 也调它。因而它既影响覆盖/排除，也用于支持目录查找。

- `TC1C` 覆盖common中的 `TC1c`；新CPU组不重复计入这两个声明。MBP13,2最终CPU候选为TC9c加TC1C…TC8C，共9个；均值的evidence保留all与后续组两处源行。
- MacPro6,1的exclude `TC0p` 也移除common `TC0P`；exclude `TG0p/TG1p` 移除common `TG0P/TG1P`。`Tm0p`不匹配 `TM0p`，因为前三字符大小写不同。
- MacPro1,1源中TM9P出现两次，后面的内存扩展板B稳压区（行124）覆盖先前出风标签，不能把重复XML节点当作同源别名。

**ARM不同。** 已有 `outputs/evidence/native/arm64-disassembly.txt` 中 `isSameSensor @0x1000458dc` 只有一条 `b 0x1001eb764`，直接跳到QString operator==。ARM `sensorByKey @0x10003d140..158` 调stored comparator，删除路径同样调用保存的比较器。因此ARM继续精确比较，Neo目录的TW0P与TW0p保留两条原始声明，不套用Intel末字符折叠。

公开给crate的 `catalog::keys_match_for_architecture(arch, declared_key, actual_key)` 只匹配元数据，不改变硬件读key。x86 `loadAvailableSensors` 的r15是实际枚举键，`0x100044d7d`传给目录lookup，`0x100044da7`转UTF8后于 `0x100044e70`调getTemperature，`0x100044ea9`构造温度对象仍传r15；标签来自目录r14+8。ARM现有 `evidence/provider-deep/pseudocode/smc-load-available.c` 也把实际pQVar18传lookup、getTemperature和对象构造。采样必须保留实际读取ID，不能将它重写成XML的字母大小写。

## 完整事实与行号

`smcSensors.xml`共213行、27个modelMatch块，其中1块为空。153个smc节点包含147个非排除声明和6个排除声明，2组、0别名；2个带范围的块含5个加号token。all共42个键候选，其中CPU组10个；排除节点仍保留原始行号参与规则对账，但不作为描述返回。源中 `MacMini3,1` 的大小写按字面保留，不能擅自改成 `Macmini3,1`。

| 源行 | models | smc节点 / 排除 / 组 |
|---|---|---|
| 3–48 | `all` | 42 / 0 / 1 |
| 49–52 | `Macmini1,1` | 2 / 0 / 0 |
| 53–56 | `Macmini3,1` | 2 / 0 / 0 |
| 57–61 | `MacBookAir2,1` | 3 / 0 / 0 |
| 62–64 | `MacBook5,2` | 1 / 0 / 0 |
| 65–65 | `MacBook8,1` | 0 / 0 / 0 |
| 66–68 | `MacBookPro2,1;MacBookPro2,2;MacBookPro3,1;MacBookPro4,1` | 1 / 0 / 0 |
| 69–71 | `MacBookPro5,1` | 1 / 0 / 0 |
| 72–78 | `MacBookPro5,1;MacBookPro5,2;MacBookPro5,3` | 5 / 0 / 0 |
| 79–81 | `MacBookPro6,1;MacBookPro6,2` | 1 / 0 / 0 |
| 82–84 | `MacBookPro5,1;MacBookPro5,2;MacBookPro5,3;MacBookPro6,2` | 1 / 0 / 0 |
| 85–97 | `Macmini8,1;MacBook8,1+;MacBookAir6,1+;MacBookPro9,1;MacBookPro9,2;MacBookPro10,1;MacBookPro11,2;MacBookPro11,3;MacBookPro11,4;MacBookPro11,5;MacBookPro13,2+` | 9 / 1 / 1 |
| 98–100 | `MacBookPro8,3` | 1 / 1 / 0 |
| 101–106 | `iMac13,2;iMac14,2;iMac15,1;iMac17,1;iMacPro1,1;MacPro1,1;MacPro2,1;MacPro3,1;MacPro4,1;MacPro5,1` | 4 / 0 / 0 |
| 107–109 | `iMac9,1;iMac10,1` | 1 / 0 / 0 |
| 110–128 | `MacPro1,1;MacPro2,1;MacPro3,1` | 17 / 0 / 0 |
| 129–156 | `MacPro4,1;MacPro5,1` | 26 / 0 / 0 |
| 157–159 | `MacPro1,1;MacPro2,1;MacPro3,1;MacPro4,1;MacPro5,1` | 1 / 0 / 0 |
| 160–179 | `MacPro6,1` | 18 / 4 / 0 |
| 180–182 | `iMac4,1;iMac5,1` | 1 / 0 / 0 |
| 183–187 | `iMac9,1` | 3 / 0 / 0 |
| 188–190 | `iMac4,1;iMac5,1;iMac6,1;iMac7,1;iMac8,1;iMac9,1;MacMini3,1` | 1 / 0 / 0 |
| 191–196 | `iMac15,1;iMac17,1` | 4 / 0 / 0 |
| 197–199 | `iMac13,1;iMac13,2` | 1 / 0 / 0 |
| 200–202 | `iMac15,1;iMac17,1` | 1 / 0 / 0 |
| 203–205 | `MacBook8,1+;MacBookAir6,2+;MacBookPro10,1;MacBookPro11,1;MacBookPro11,3;MacBookPro11,4;MacBookPro12,1;MacBookPro13,1;MacBookPro13,2;MacBookPro13,3;MacBookPro14,1;MacBookPro14,2;MacBookPro14,3;MacBookPro15,1;MacBookPro15,2` | 1 / 0 / 0 |
| 206–212 | `iMacPro1,1` | 5 / 0 / 0 |

## 验证与剩余边界

独立双模块测试，不编译Tauri：

```sh
cat > /tmp/mfc-catalog-harness.rs <<EOF
#[path = "$(pwd)/outputs/macs-fan/src-tauri/src/catalog.rs"] mod catalog;
#[path = "$(pwd)/outputs/macs-fan/src-tauri/src/intel_catalog.rs"] mod intel_catalog;
EOF
rustfmt --edition 2021 --check outputs/macs-fan/src-tauri/src/catalog.rs outputs/macs-fan/src-tauri/src/intel_catalog.rs
rustc --edition 2021 --test /tmp/mfc-catalog-harness.rs -o /tmp/mfc-both-catalog-tests
/tmp/mfc-both-catalog-tests
```

20项测试全部通过（ARM原12项和Intel新增8项）：覆盖精确型号、包含边界的范围、格式/溢出拒绝、排除与覆盖冲突、末字符等价与前三字符区分、源重复节点、架构隔离、未知Intel common、原ARM/Neo行为、唯一均值成员与源行证据。独立Python逐块对账27个models顺序、153个key/行号、6个exclude属性存在性、0别名和2组的源行/成员/hide，全部一致。

静态资源装载、比较器及支持查找链已确认；x86未全量反编译，也未在Intel硬件或Rosetta实测。原程序CPU信息过滤、传感器枚举可见性、大小写键实际共存、读数质量、聚合采样策略和其他provider仍是独立边界。这些测试只验证目录行为，不证明任一机器具备所有候选键。主程序应自行注册 `mod intel_catalog;` 并采用明确架构入口；本scope未改monitor/main/UI/HID。
