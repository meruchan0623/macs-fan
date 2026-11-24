# Macs Fan

独立重建的 macOS 风扇与温度监测应用，Tauri 2 + Rust + 无框架本地前端。源码与原创图标均在此目录。原程序的研究证据在上一级，不参与构建。

当前版本是**只读监测**，不是完整风扇控制替代品：不写 SMC、不调用 CrystalIDEA helper、不申请 root。预设可保存、恢复和编辑；保存不会改变风扇转速。当前支持 SMC、IOKit HID 与只读SMART磁盘来源；已纳入46条机型归一化、ARM与Intel独立目录。Intel GPU registry适配已实现并通过注入测试，尚缺Intel实机与x86 Rust构建验收；ARM不调用该来源。ATA/NVMe持续监测已接入，当前worker内30秒采样、45秒过期；本机NVMe验证成功，ATA仍缺厂商实机验证。未知温度键不冒充已识别CPU/GPU。硬件控制需要完成 docs/control-design.md 的验证门槛。

## 构建

macOS、Xcode Command Line Tools、Rust 和 Node/npm。依赖版本由 Cargo.lock/package-lock.json 固定。

```sh
npm ci
npm run build
```

如希望把编译缓存放在项目外，可设置 `CARGO_TARGET_DIR`。产物在相应 target/release/bundle/macos/Macs Fan.app。

```sh
cargo test --manifest-path src-tauri/Cargo.toml
node scripts/test-ui.mjs
node scripts/test-control.mjs
cargo run --manifest-path src-tauri/Cargo.toml -- --snapshot
```

`--snapshot` 执行真实只读采样，并输出JSON。启动 GUI 不加此参数。前端可用任意静态 HTTP 服务查看，无 Tauri 时明确显示不可用；只有显式 `?demo=1` 才会进入带标识的演示页面。

## 签名

本机没有有效签名身份，因此当前配置显式使用 `signingIdentity: "-"`，仅 ad-hoc 本地签名，不是 Developer ID、不公证、不保证跨机器 Gatekeeper 放行或持久 TCC 身份。没有创建证书、改变系统信任或关闭安全功能。

获得身份后通过环境变量 `APPLE_SIGNING_IDENTITY` 与 Tauri 官方流程签名/公证；不要把密钥或密码写进项目。当前 UI/后端无需 Accessibility 或录屏权限。

## 安装

确认构建通过后，把生成的 `Macs Fan.app` 复制到 `/Applications`。保留原 `Macs Fan Control.app`。如已存在同名新应用，应先退出并备份后再替换，不覆盖未知版本。

## 数据和行为

所有采样在本机完成；无外部网络、遥测、账户或许可服务。诊断复制由用户主动触发。显示源键、失败原因和读取时间；错误不会被伪装为零 RPM 或实时旧温度。SMC 属于未公开稳定保证的接口，需在目标设备与系统版本上验证。

原生应用已取得本机真实读数；具体运行、浏览器和构建验收记录见上一级 delivery-status.md。浏览器演示数据单独标记。

## 已知运行边界

SMC 读取在独立子进程执行，6秒超时后清空界面并请求终止/回收 worker，确认回收后才允许重建。无法保证终止不可中断的内核等待；此时停止重建，详见 docs/sampling-isolation.md。目录部分失败60秒后重试，成功目录300秒刷新，连接失败清缓存。快照时间取采样开始，避免慢采样掩盖较早的读数。`quality=valid` 表示成功解码并落在显示区间，不表示原始键的物理含义或测量准确性已经验证。

## 0.5 检测与界面

概览显示CPU/GPU派生均值、可用成员及常用部件，部件视图显示资料映射的键，全部视图保留所有原始候选。名称映射只表明资料关联，不表示位置经过独立校准；证据随每个映射传感器一起进入诊断JSON。

ARM精确目录均值要求全部声明成员在本次读取有效；Intel通用组和HID按已发现成员计算且标明覆盖范围。零值或ARM Tp<11°C保留但标为可疑，不补算、不随机修正。若均值不可用，概览显示有效成员，原均值仍为“—”。TCDX/TCMb仅作带来源注记的CPU参考读数，不作为核心值或均值替代。

模式先按存在的FS!协议读取，否则探测大小写直接模式键。未知值及直接键冲突均显示未知，源键/原值可在行提示和JSON查看；FNum=0事实不再掩盖温度采样错误。采样IPC上限4MiB，并有4096键、16风扇和极端诊断文本的序列化边界测试。

control-core/、helper/、smc-adapter/分别为独立事务核心、非特权XPC验证工程和受准入限制的SMC执行器。0.7将只读helper与固定daemon描述符封装进应用，但未注册服务，当时GUI尚未建立生产XPC控制连接；真实写入允许表为空，不能把模拟测试视为可用调速功能。

窗口默认560×440，最小460×340。关闭窗口后保留菜单栏入口，左键切换显示，右键可显示/隐藏/退出；Dock再次打开也可唤回。菜单栏创建失败时走窗口关闭/草稿确认路径。传感器别名只进入搜索和悬停详情，不增加重复行或重复均值权重。

0.5新增IOKit HID独立来源、Product/registry身份和Intel目录。空事件不转成0，重复Product不静默覆盖或平均；Intel通用组与HID组按已发现源计算，详见docs/multiple-providers.md。界面尺寸不变，原始源仍在“全部”，不新增说明区。

## 0.5.1 风扇位置

风扇行按机型资料显示左右侧、进风或排风位置，未知机型或索引仍保留编号。实际通道数来自 FNum，不由名称推导；“双风扇”组件仍只对应枚举到的通道。位置出处在诊断 JSON，悬停显示资料匹配限制。窗口尺寸和常驻布局不变，详见 docs/fan-catalog-evidence.md。

## 0.6 预设与显示设置

主界面仍为560×440，最小460×340。底栏“预设”打开紧凑编辑器：新建、改名、删除、自动/固定RPM/传感器曲线配置及预览；保存与硬件应用分离。齿轮打开温标和精度；范围及排序一并持久化。

独立配置位于 macOS Application Support/local.macsfan.desktop/preferences.json，采用稳定随机ID、版本化文档、修订冲突检测、文件锁及原子替换。文件0600、目录0700；损坏/未来版本拒绝覆盖。没有迁移或读取原应用用户配置。详见 docs/preferences-contract.md 与 docs/preferences-storage.md。

编辑中切换、关闭和应用菜单⌘Q会先确认放弃；取消保留草稿。⌘Q的原生运行验证已通过。Dock直接退出、强制退出/崩溃不承诺草稿恢复；界面卡死时也可能需要强制退出。保存仅提交配置，不向硬件应用。

helper现已连接独立SMC只读适配器，跨进程验证真实枚举、逐键inspection与未验证控制请求拒绝。该测试通道独立运行；0.7另将默认拒绝启动的生产只读宿主封装进应用，不改变监测应用的权限或主界面。详见helper/docs/readonly-integration.md。

## 0.7 服务封装

新增固定身份的只读生产宿主和ServiceManagement桥接。诊断按需读取服务状态，主界面没有新增安装按钮或说明区。macOS 11/12返回unsupported；13+查询固定服务，签名与包验证通过后才允许显式注册/卸载。当前Team源码常量为空，本机无发行身份，注册不可用。helper被封装不等于已安装系统服务。

在未安装的构建产物上追加打包：

```sh
python3 helper/scripts/stage_service.py --app '/path/to/target/release/bundle/macos/Macs Fan.app'
```

脚本构建无test-harness的release静态库、编译固定宿主、先签helper再签应用并严格校验，不执行注册、root或安装。默认ad-hoc；只传入发行证书仍不足以启用服务，必须审核固定Team、双向身份与实际运行。构建设置CARGO_PROFILE_RELEASE_STRIP=none，避免本机Rust/LLVM剥离导致Mach-O LINKEDIT对齐错误；没有修改编译器二进制或关闭系统验证。参考[LLVM问题203678](https://github.com/llvm/llvm-project/issues/203678)。

`Macs Fan.app/Contents/MacOS/macs-fan --service-status`为只读状态CLI。生产宿主、身份和服务桥接的边界见helper/docs/production-host.md、service-management.md；真实安装/批准/root/XPC生产连接仍待正式身份和现场验收。

## 0.8 认证只读检查

诊断现在分别查询服务管理状态与固定helper检查，前者成功不代表后者已认证。原生连接器仅发送inspect_hardware，固定服务名、客户端签名要求和服务端要求；没有前端提供的任意请求/身份参数。缺少Team仍立即返回blocked。新增只读CLI为`Macs Fan.app/Contents/MacOS/macs-fan --helper-inspect`。

原生侧为回复设置3秒单调截止，迟到结果不采纳，断线/异常有固定错误，重入立即返回busy。前端每端最多保留一个真实请求，4秒后显示超时但不会因此重复启动未结束的原生调用；每端分别发布结果和完成时间。没有新增主界面说明或放开控制。

真实非特权NSXPC测试夹具验证新连接器协议与双向拒绝路径；它只返回固定无硬件错误，使用仅测试的身份策略。生产固定Team、root服务和硬件调速仍未验收。详见helper/docs/production-client.md。原生构建必须保留-fobjc-arc-exceptions，确保捕获Objective-C异常时释放强引用；不使用会使Rust最终链接缺少运行时符号的@available，旧系统在新selector之前显式拒绝。

## 0.9 温控执行基础

`control-core`新增同一租约内的SetTarget目标更新：绑定owner/token/递增seq，验证能力与当前归属，读回后才提交目标/期限；失败恢复识别原目标与尝试目标，遇外部接管退让。helper新增闭合set_target协议，真实只读后端仍无法取得租约或写入。

独立`control-runtime`实现固定/传感器/自动计划的持续软件执行、全计划预检、续租/目标更新、多风扇失败回退、样本新鲜度、独立看门狗与生命周期停止。它内建全新引擎，注入单调时钟并在I/O前后重新检查，状态区分实际自动读回、恢复等待/耗尽、退让及未知。共享曲线实现位于control-runtime/src/policy.rs；主程序原策略测试仍使用这份源。

0.9当时Coordinator尚未接入GUI或生产宿主；0.10接入宿主，0.11补齐GUI显式计划调用。真实调速仍被生产门禁禁止。将其接入真实控制还需服务端可信传感器、生产会话/生命周期调度、已审核可写机型以及失效恢复。独立Fake执行测试不是硬件或热安全证明。用户已确认没有Developer ID，正式签名/生产现场验收继续保留为外部条件。

## 0.10 宿主计划接线

helper现以Coordinator维护唯一引擎账本；固定计划通过原有NSXPC宿主和独立timer执行，客户端授权独立到期。实现、wire和边界见[helper/docs/host-plans.md](helper/docs/host-plans.md)。原始租约与计划互斥；温控计划缺helper本地可信采样，返回明确错误。GUI仍只读、保存预设不应用，生产身份和真实写入准入仍未满足。

## 0.11 显式应用与停止

紧凑预设编辑器加入“应用 / 停止”，保存仍不启动控制。应用要求已保存且显示内容与当前文档一致；并发修改后更新干净编辑器，保留未保存草稿。Rust重新读取准确修订，校验本机与完整风扇清单；原生持久连接负责独立续租、停止取消、截止及迟到回复隔离。详见[docs/control-bridge.md](docs/control-bridge.md)和[helper/docs/control-client.md](helper/docs/control-client.md)。

正式Team缺失时应用按钮禁用；即使通过身份，当前只读helper也拒绝计划启动。原生客户端测试使用隔离Fake，不是实际调速。新增只读CLI `--control-status`；主界面仍560×440、最小460×340，没有增加常驻说明区。

## 0.12 服务端采样与准备状态

helper新增独立采样worker和有界单槽批次，服务端自行赋予来源ID、准入质量和同epoch读取开始时间。没有新样本不重打时间戳；失败/异常/挂起会终止计划，状态查询不能代替独立timer。Fake宿主的test:temperature完成准备、执行、停止整条协议链路；生产仍没有已校准温度来源，真实写入继续关闭。

smc-adapter新增独立TemperatureReader，只读真实T键、核对类型/大小/身份，返回明确未校准候选；probe已读取本机四键。它没有写接口，也不会把候选升级为控制输入。原生客户端和UI增加Preparing证据状态，窗口大小和布局不变。见helper/docs/local-sampling.md、smc-adapter/docs/temperature-reader.md。

## 0.13 GPU来源与故障显示

新增Intel GPU registry只读adapter，按实际属性owner ID保留独立来源，缺失不填零、失败不重用旧温度。Apple Silicon明确not_applicable。原生空CF释放、成功空枚举状态以及不可用GPU覆盖原始概览回退的问题已修复，详见docs/gpu-provider.md与上级两份0.13审查报告。未改变窗口尺寸或新增常驻说明。110项主程序测试、严格Clippy、前端逻辑及独立故障注入通过；无Intel实机验收。磁盘SDK合同与一次非特权NVMe读取证据在../evidence/disk-probe-0.13，未纳入本应用持续采样或控制来源。

## 0.14 磁盘持续监测

新增SDK C＋Rust只读ATA/NVMe适配，与SMC/HID保留独立来源。不启用SMART、不执行自测、IDENTIFY或写操作，不采集序列号。缓存保留read-start时间，失败清空读数；逐源sampledAtMs/expiresAtMs不会随其它来源刷新而续期。前端在过期、时钟回拨或恢复时更新表格，不增加常驻说明。详见docs/disk-provider.md及docs/source-freshness.md。

`--snapshot`现在也经既有隔离worker执行，错误和成功输出前均请求回收；不保证底层IOKit可取消或同步回收完成。124项主程序测试与严格Clippy通过，原生/合并两路独立审查关闭重要问题。发布版不含C测试main或helper测试入口。ATA按常见194/190约定解码，仅为未校准候选；NVMe本机通过不代表外置桥、其它驱动或真实睡眠验证。

## 0.16 控制软件接线与候选采样

新增受内部编译profile约束的控制构造和七种封闭能力组合；客户端在同一认证连接上核验能力，跨回复变化、过期或断连撤销写能力声明。显式readonly构造和失败降权不可提升。**生产入口仍选择readonly，真实profile表为空**，这些软件接线不表示本机可以调速。详情见[production-control.md](helper/docs/production-control.md)。

宿主独立worker已接固定Mac16,8/aarch64四键候选，初始化与读取不占控制线程，停止/迟到结果丢弃。候选始终Unverified，不成为温控来源；当前仅内部批次可观察，未扩充GUI或wire诊断。见[candidate-sampling.md](helper/docs/candidate-sampling.md)。

控制界面新鲜度使用performance.now单调时钟；异常回退锁定旧确认，新的有效回复才重建基线。停止后新鲜的自动模式读回仍可确认，不要求保留可写连接；过期则撤销确认。窗口尺寸与按需弹窗保持不变。
