# AxVM 生命周期 API

AxVM 为每个 VM 实例创建一个控制任务。调用方持有 `VmHandle`，提交命令并通过
`VmOperation` 分别观察接受与完成；生命周期、资源转换与运行代次只由控制 owner 修改。
实现细节见 [生命周期实现](lifecycle-internals.md)，总体边界见
[所有权与锁分层](../../../docs/design/axvm-ownership-lock-boundary.md)。

## 1. 实例与操作

`VmManager` 拥有实例注册表、宿主初始化与跨 VM 服务。VM ID 可复用，因此调用方应同时保留
`VmKey`；每次运行使用新的 `RunId`。接口定义位于 `src/manager.rs` 与 `src/identity.rs`。

### 1.1 创建与查询

`VmManager::create(VmCreatePlan)` 消费配置、准备好的启动方案与镜像提供者，返回
`VmOperation<VmHandle>`。控制任务完成资源建立与镜像装载后发布 `Ready` 句柄；注册表先
保留 ID，拒绝重复创建，再发布已完成的句柄。查询 `get`、`list` 只复制句柄，锁外调用 VM 操作。

| 对象 | 调用方获得的能力 |
| --- | --- |
| `VmKey` | VM ID 与 manager 分配的实例代次 |
| `RunId` | 一个实例中的运行代次；重启、reset 后改变 |
| `VmHandle` | 命令端点与查询快照，不包含可变硬件后端 |
| `VmSnapshot` | owner 发布的状态、当前操作、最后错误、vCPU 观察值与运行计数 |

旧句柄始终指向原实例。成功销毁后该实例从注册表注销，同 ID 的新 VM 使用新的 `VmKey`，
旧操作、IRQ 端口与 vCPU 确认不能修改它。

### 1.2 接受与完成

生命周期方法返回 `AxVmResult<VmOperation<T>>`。外层错误表示无法提交命令；
`accepted().await` 表示 owner 已校验并开始处理，操作的 `.await` 或 `wait()` 返回最终结果。
`OperationId` 标识本实例中的具体操作，拒绝与执行失败均沿用 `AxVmError`。

```rust
let operation = vm.stop(StopReason::Clean)?;
operation.accepted().await?;
operation.await?; // vCPU、设备与运行服务已静默并回收
```

丢弃观察句柄不取消 owner 的操作。同步 `wait()` 只用于可睡眠任务上下文；异步 handler 使用
Future。句柄析构不阻塞销毁。调用方的超时或断开不证明资源已静默，不能据此释放 backing。

## 2. 状态与完成条件

`VmStatus` 定义于 `src/lifecycle/status.rs`，由控制任务发布到快照。过渡状态可被查询，
稳定状态表示对应完成条件成立；硬件执行进展另由运行计数观察。

### 2.1 稳定与过渡状态

owner 通过 `Pausing`、`Stopping` 表达尚未收齐参与者确认的操作，不能把它们当作完成结果。
`Failed` 保留失败信息与仍需回收的资源，后续停止或销毁可以重试清理。

| 状态 | 含义 |
| --- | --- |
| `Ready` | 资源与启动内容已准备，尚未打开客户机准入 |
| `Running` | owner 初始化／恢复、设备与信号路径就绪，客户机准入已打开 |
| `Pausing` | 已关闭准入，等待 vCPU park 与设备静默 |
| `Paused` | 参与 vCPU 已卸载并 park，设备后台执行已静默 |
| `Stopping` | 停止与资源退休正在进行 |
| `Stopped` | 本运行的 vCPU、后台工作与端口已退休；实例保留，可重新启动 |
| `Destroying` | 状态类型保留的销毁阶段；不能据此认定任务已退出 |
| `Destroyed` | owner 已提交销毁；操作完成还包括资源释放、注销与任务退出确认 |
| `Failed` | 操作或补偿失败；准入关闭，资源仍有明确 owner |

`Running` 不保证客户机已执行第一条指令。`Destroyed` 的快照发布也不能替代 destroy 操作
的完成观察；完成由宿主任务退出／资源退休链确认。

### 2.2 操作后置条件

`start` 允许 `Ready`、`Stopped`，`pause` 允许 `Running`、`Paused`，`resume` 允许
`Paused`、`Running`；重复 pause／resume 已达到目标时成功。停止、reset、destroy 通过 owner
顺序处理；reset 先停止旧运行，再建立新运行。

| 操作 | 完成结果 | 后置条件 |
| --- | --- | --- |
| `create` | `VmHandle` | 资源、设备与镜像已准备，状态为 `Ready` |
| `start` | `RunId` | 必需 owner 已初始化、设备与路由就绪、准入打开且 owner 已唤醒 |
| `pause` | `()` | 所有参与 vCPU 卸载并 park，设备后台执行已静默，状态为 `Paused` |
| `resume` | `()` | 设备恢复、保留 work 可消费、准入打开且 owner 已唤醒 |
| `stop` | `()` | 本运行已静默、join 与退休完成，状态为 `Stopped` |
| `reset` | 新 `RunId` | 旧运行停止与回收，新运行满足 start 后置条件 |
| `destroy` | `()` | 资源释放、该 `VmKey` 注销、控制任务退出得到确认 |

`start` 从 `Stopped` 与 reset 都重新准备运行对象和启动内容，不从上次指令恢复。
暂停恢复保留寄存器与客户机内存；宿主单调时钟继续前进，到期 timer 在恢复时重新评估。
`StopReason` 记录原因，不改变静默与退休要求。

## 3. 并发与失败

普通管理命令按 owner 接收顺序执行。等待执行确认时，owner 继续处理内部事件与 guest 请求，
避免确认依赖 owner 本身却被同步等待阻塞。

### 3.1 重试与回滚

同运行期重复 stop 共享停止完成，已停止时成功；重复 destroy 在原实例上观察同一退休结果。
启动失败关闭准入并回收已发布 startup。pause／resume 的设备步骤失败执行逆向补偿，补偿失败
进入 `Failed`。清理失败保留注册项、关闭的运行对象和 backing，后续清理重试不会提前释放。

错误由操作结果与 `VmSnapshot::last_failure` 提供。`StaleRun` 表示请求运行代次不匹配，
`OperationCancelled` 表示操作依赖的参与者已取消，`EntryClosed` 表示实例入口关闭。
观察者超时不取消任务，也不授权跳过 join、IRQ 退休或 DMA 静默。

### 3.2 查询与执行进展

`VmHandle::snapshot` 复制 owner 发布的生命周期与对应激活集合，再在锁外读取
`VcpuProgress` 原子计数。已退休激活的计数纳入该运行基数；旧观察集合只能读取原激活，
不能混入 reset 或 ID 复用后的运行。

`entry_count` 在一次真实硬件执行返回 exit 后递增；`EngineOutcome::Interrupted` 重试不递增。
`park_count` 记录参与者完成卸载／暂停。它们是 VM 级进展信号，不能替代逐参与者确认与设备
静默契约；停止后退休该运行观察集合，重新启动从新运行计数开始。

## 4. 控制面适配

Axvisor 在 `os/axvisor/src/manager.rs` 持有实例化 `VmManager`，将 TOML 和镜像来源转换为
`VmCreatePlan`。Shell 与 HTTP 通过同一 `VmHandle` 提交生命周期命令。

### 4.1 同步任务入口

shell 可在任务上下文调用 `wait()`。完整生命周期用同一个句柄完成，销毁无需另行全局注销：

```rust
let vm = manager.create(plan)?.wait()?;
let run = vm.start()?.wait()?;
vm.pause()?.wait()?;
vm.resume()?.wait()?;
vm.stop(StopReason::Clean)?.wait()?;
vm.destroy()?.wait()?;
```

`run` 可用于校验内部内存更新的预期运行。注册表锁、mailbox 锁和快照锁不跨上述等待持有。
`VmManager::shutdown` 关闭创建入口，发出所有销毁请求，在注册表锁外等待并回收任务。

### 4.2 异步 HTTP 入口

HTTP 的 pause／stop 在 owner 接受后响应，调用方轮询终态；create、start、resume、delete
等待操作 Future 完成，handler 不使用阻塞 `wait()`。响应形状保持原契约。

真实暂停／恢复、Stopped 后启动、销毁后重建与执行进展由现有
`http-control-plane` QEMU 用例验证。该系统证据与组件的并发、取消、确认和失败测试分别保护
实际宿主调度链及受控交错，不能互相替代。
