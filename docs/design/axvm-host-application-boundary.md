# AxVM 宿主与应用边界

Axvisor 决定配置、管理入口、控制台前台和宿主资源交接策略。AxVM 的 `VmManager`、控制 owner 和 vCPU owner 实现资源创建、生命周期转换与硬件执行。公共入口只暴露操作观察、快照和运行代次绑定的窄端口，TOML 与 guest ABI 保持现有格式。

## 1. 应用所有权

应用通过实例化 manager 管理 VM，不能从线程附件或 VM ID 全局查询取得可变后端。`os/axvisor/src/manager.rs` 的 `AxvmManager` 拥有 `axvm::VmManager`；应用自己的 `OnceLock<Arc<AxvmManager>>` 只保存这个应用实例。

### 1.1 生命周期调用

`create_vm_from_toml` 准备 `VmCreatePlan`，`VmManager::create` 在注册表内预约 ID，再由每 VM 控制任务创建资源、装载镜像和发布 `Ready` 句柄。`VmHandle` 的操作经 mailbox 到达唯一 `control::Owner`，管理调用不持注册表锁等待。

| 调用方 | 操作观察 | 成功依据 |
| --- | --- | --- |
| shell | `VmOperation::wait` | 对应操作后置条件成立 |
| HTTP pause / stop | `accepted().await` | owner 校验并接受请求 |
| HTTP start / resume / delete | 操作 `.await` | 完成确认已发布 |
| 同步 delete / shutdown | destroy 完成后 join | 控制任务退出及管理句柄回收 |

HTTP handler 不调用同步 `wait` 或 `join`。`VmSnapshot` 提供配置、状态、运行代次、当前操作与失败观察，快照不引用 owner 的资源。句柄析构不隐式销毁 VM；清理失败保留资源并允许重试。

### 1.2 控制台服务

`GuestConsoleMux::ConsoleCore` 的 `state` 和 `output_lock` 使用真实 `std::sync::Mutex`。调用发生在卸载硬件后或应用任务中；hard IRQ 不进入 mux。双锁顺序是 `output_lock` → `state`，通知 VM 在两把锁释放后执行。

物理输入仍只有一个 `TaskConsoleInput` owner，输出通过既有有界 ordered record queue 和输出 worker。可睡眠 mutex 只串行化任务状态，不能带进 guest-entry、IRQ-off 或 CPU-local 后端作用域。

## 2. 宿主能力

AxVM 的 `host` 模块提供应用确实需要的宿主操作，HAL 和驱动类型留在私有 ArceOS 适配器内。虚拟化、IRQ、CPU 及翻译操作通过 `ax_std::os::arceos` 扩展调用。

### 2.1 窄应用入口

`host::console` 控制宿主控制台的 task capability，`host::cpu` 报告读取任务选核需要的 CPU 拓扑。文件系统释放、块设备准备和具体中断路由属于 AxVM 的宿主适配器，公共函数使用 AxVM 类型或普通数据。

Axvisor 的板级配置直接选择 `ax-driver/<feature>`；可选依赖作为 Cargo feature 路由锚点，应用源代码不调用驱动实现 API。内部 `HostCpu`、`HostMemory`、`HostTime` 和 `HostPlatform` 不扩大为应用公共 HAL。

### 2.2 直通交接

`AxvmManager::release_host_filesystem_for_guest_passthrough` 在 guest 取得直通设备前关闭宿主文件系统并准备宿主块设备。x86 的 `ArchOps::enter_runtime` 为本次 `RunId` 建立 INTx 转发，路由绑定成功后才启用 producer，退出时先关闭准入并 kick，再注销与等待静默。

路由中的 hard IRQ target 是预绑定的 `X86DeliveryPort` 或对应架构 native port；它不查询 VM 注册表或运行期设备集合。交接失败按既有错误语义返回，不能在清理未确认时释放 backing。

## 3. Rust 同步层级

AxVM 使用真实 Rust `std`，生产构建由 axbuild 把架构请求映射到相应 RustStd/musl PIE target，并构建 `std + panic_abort`。集合、线程、原子、`OnceLock` 和任务状态 mutex 均使用真实 `std` 接口。

### 3.1 标准名称

锁名称表示阻塞类别，所属模块说明 poison 和 PI 差异。AxVM 普通资源状态使用 `std::sync::Mutex`；原生锁由 `ax_std::os::arceos::sync` 提供。

| 名称 | 允许上下文 | 获取语义 |
| --- | --- | --- |
| `Mutex` / `RwSemaphore` | 可睡眠任务 | 竞争可以睡眠 |
| `RtSpinLock` / `RtSpinRwLock` | 原生可抢占任务 | 竞争可以睡眠，持锁约束迁移 |
| `RawSpinLock` / `RawSpinRwLock` | 不可睡眠短状态 | 普通获取禁止抢占，IRQ-save 获取另存 IRQ |
| `LocalLock` | 本 CPU 本地状态 | 使用明确的上下文获取接口 |

`RawSpinLock::lock_raw` 不改变上下文，并要求调用方满足其 unsafe 契约。`IrqSafeMutex`、`NoPreemptMutex` 和其他同义别名已删除；IRQ 与抢占策略由获取方法说明。

### 3.2 执行所有权

`OwnedVcpuEngine::run_once` 在一个独占后端上完成加载、Completion 提交、pending 消费、入场复查、硬件执行和退出捕获。返回任务层前卸载后端并恢复 CPU 与 IRQ 上下文，`ArchitectureExitHandler` 此后才能访问设备和客户机内存。

CPU-local `ExecutionContext` 只保存执行身份、预绑定 `VcpuSignals` 和受作用域约束的 decode view。线程附件只暴露 `VcpuInstance`；可变后端通过拥有值转移，退出和 join 确认后才回到控制 owner 的 slot。

## 4. 验证范围

同步边界既需要接口与依赖核对，也需要真实调度和硬件证据。源码命名或构建成功不能证明进入竞态、IRQ ACK/EOI 或 DMA 静默。

### 4.1 组件证明

项目任务工具验证 native / bridge 锁算法、owner 完成观察、陈旧确认、部分失败和资源退休。设备宿主测试使用正式能力接口的测试环境，验证设备行为与 wrapper 契约；不替代原生 PI、IRQ 恢复和调度验证。

### 4.2 目标证明

四架构 Axvisor 构建验证实际后端装配；可用 QEMU 或板卡运行验证 CPU_ON、生命周期、timer、IRQ、块设备、IVC、HTTP 与控制台。VMX 和 SVM 分别记录，缺少硬件的目标明确列为未验证。详细协议与完成条件见 [AxVM 所有权与锁边界](./axvm-ownership-lock-boundary.md)。
