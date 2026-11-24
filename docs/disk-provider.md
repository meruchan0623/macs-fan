# 只读磁盘温度 provider

`src-tauri/native/DiskTemperature.c` 使用本机 Apple SDK 具名 `IOATASMARTInterface` / `IONVMeSMARTInterface`，Rust `src/disk.rs` 只接收固定结构和原始 512 bytes。只在现有隔离采样 worker 中调用 `disk::sample()`；本模块没有线程、缓存、节流或控制准入。外层负责 30 秒采样缓存、来源时间和过期展示。

## 证据与原实现差异

静态恢复源为 Macs Fan Control 1.5.21 universal；证据在仓库 `outputs/extra-providers-analysis.md` 和 `outputs/evidence/disk-probe-0.13/sdk-contract.md`。arm64 原地址：磁盘枚举 `0x10003e59c`、能力检查 `0x10003ebe8`、加载 `0x10003ed44`、plugin `0x10003f8e0`、采样 `0x10003fcd8`、ATA解析 `0x10003e0a8`、NVMe解析 `0x10003e108`。

- 每次重新枚举 `IOBlockStorageDevice`。仅接受 CFBoolean true 的 `NVMe SMART Capable` / `SMART Capable`；NVMe优先。没有照搬原实现的“属性存在即有能力”，也没有实现可选 ATA Features bit5 fallback。
- 每次创建plugin → QueryInterface → SMARTReadData → 接口Release → IODestroyPlugInInterface。IOCreatePlugInInterfaceForService 参数顺序为 service、user-client UUID、kIOCFPlugInInterfaceID。失败分支也释放已获取对象。
- 原ATA实现先enable再read；本实现只read，不调用SMARTEnableDisableOperations、SMART自测、写命令、IDENTIFY、厂商helper，不提权。没有读取用户磁盘文件、BSD路径或序列号。名称只读取 `Device Characteristics.Product Name` 的CFString；名称缺失由UI显示“磁盘”。
- 数据仅在read成功后进入后续处理。ATA还要求SDK SMARTValidateReadData成功，再由Rust独立核对512 bytes加和mod256==0。194优先，完全不存在194才退190；raw位置`7+12*i`。194为0不会退190。重复属性值冲突输出suspect且无温度，不选择冲突值。无冲突时按常见194/190 raw首字节温度约定解码；SDK把这一区域定义为vendorSpecific1，尚未针对具体ATA厂商验证格式。valid只表示该约定及数值检查通过，不表示物理校准或厂商覆盖得到验证。
- NVMe只解码offset1的u16LE composite Kelvin，减273.15保留小数。Kelvin0、ATA raw0为unavailable；不生成默认0°C。有限且处于[-40,150]°C才提供温度。与原程序不同，不无限保留旧值、不按整数舍入、不升级为控制源。

## ABI与边界

ABI v1，所有字段固定宽度，C/Rust均静态验证size与关键offset。`mf_disk_sample(MFDiskBatch*, uint32_t capacity)`只接受精确10784 bytes，返回0表示调用合同成功，枚举结果在结构内。未知ABI/错误size/超限计数由Rust拒绝。

| 结构 | 字段与偏移 | size/align |
|---|---|---|
| MFDiskReading | registry_id:u64@0；transport:u32@8；stage:u32@12；status:i32@16；validation_status:i32@20；raw_valid:u32@24；reserved:u32@28；model[128]@32；raw[512]@160 | 672/8 |
| MFDiskBatch | abi_version:u32@0；size:u32@4；reading_size:u32@8；supported:u32@12；service_count:u32@16；reading_count:u32@20；truncated:u32@24；enumeration_status:i32@28；readings[16]@32 | 10784/8 |

transport 1=ATA、2=NVMe；stage 1=open、2=query、3=read、4=validate、5=complete。`raw_valid=1`只在read成功、ATA validator也成功时设置；失败不复制临时SMART buffer。C输出最多16条记录，最多处理256个service；Rust最多32条diagnostics、每条256个Unicode字符。registryID为0或获取失败时省略并标truncated；同ID去重。迭代器失效记录offline和truncated，枚举失败不会伪装成空成功；成功枚举且没有候选设备为空数组。SDK成功但返回NULL iterator也表示无匹配，保持available=true。macOS原生调用失败或ABI不符保持supported=true、available=false并保留诊断；native.supported非1视为协议错误。

`source_id = disk:{ata|nvme}:registry:{16hex}`只对IORegistry service的生命期有效，不能视为跨boot硬盘身份。不采集序列号。`captured_at_ms`表示进入本次同步原生I/O前的采样开始时间（失败时也保留）；外层缓存必须保留它，而不是给缓存值重新打时间戳。

## 权限与实机范围

在本机Apple Silicon、普通用户下直接调用新实现成功：2个service、1个NVMe候选、没有truncated，APPLE SSD AP0512Z composite 299K = 25.85°C。ATA没有可用实机，本轮只验证SDK编译、合成buffer和失败路径。macOS11 deployment下arm64与x86_64均完成SDK编译检查；不表示已经在Intel硬件验证。

SMART访问可能因权限、桥接器、设备拔出、独占访问或驱动支持而失败。同步IOKit调用可能阻塞，模块内部没有安全的硬超时取消；依赖既有worker进程隔离和外层超时终止。不能为修复blocked I/O而在主进程调用、提权或增加SMART enable操作。

## 验证入口

Rust可独立 `rustc --test src-tauri/src/disk.rs` 链接C object及IOKit/CoreFoundation，避免占用主Cargo target。7项测试覆盖NVMe小数/零/范围、ATA优先级/零/checksum/冲突、SDK校验失败、失败buffer不消费、identity去重/省略、ABI和枚举失败、采样前时间戳和失败时保留、ABI失败不隐藏为平台不适用。

C使用`-DMF_DISK_TEST_MAIN`编译同一源文件的测试main，内部`DiskAPI` seam注入create/read/destroy。测试open权限失败、query接口失败、read离线且返回脏buffer时不复制字节、接口Release/plugin destroy，以及null/错误capacity拒绝、枚举成功+NULL iterator保留空成功语义。测试main和fake接口不进入production build。生产的固定SDK API表不由外部输入替换。

本轮通过：7/7 Rust单测、C故障注入测试、arm64/x86_64 SDK编译检查，以及一次非root新provider实读。未执行原程序或原厂helper。
