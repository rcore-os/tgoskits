# RKNPU Direct DMA 提交恢复与权限边界

## 问题与方案

提交 [`3b769254b0fb3fb49215f4141cd909fae8ce4ef9`](https://github.com/rcore-os/tgoskits/commit/3b769254b0fb3fb49215f4141cd909fae8ce4ef9)
（2026-09-12，PR #2327，`fix(rknpu): copy and validate user task arrays before NPU submit`）
要求 translated IOMMU 才能提交用户任务，而 RK3588 当前采用 Direct DMA，
且 GEM CPU 映射未支持 translated domain，导致 RKNN 初始化后第一次 run 被拒绝。
另一个 ABI 问题是 `task_obj_addr` 被当成普通用户地址复制：vendor runtime 实际传回
MemCreate 的对象标识。不能用移除全局检查或直接解引用该值解决。

本次恢复已有可信机器人工作负载，复用 RGA 已有的 `CAP_SYS_RAWIO` 权限：
每次 ioctl 都检查当前进程的有效能力，降权后的继承 fd 也不能提交。
无权限返回 EPERM；这是一项明确的权限限制，不声称普通用户有 IOMMU 隔离。
通用驱动的安全 `submit_ioctrl` 仍拒绝 Direct DMA；新增明确的 unsafe rawio 入口，
由 Starry 完成权限验证后调用，不把 IOMMU 状态伪造成启用。

## ABI、内存与所有权

对照 Rockchip Linux develop-6.1 固定提交
`77168c8d5ab82399f65a80e9f807b50ba37cf483` 的
[`rknpu_job.c`](https://github.com/rockchip-linux/kernel/blob/77168c8d5ab82399f65a80e9f807b50ba37cf483/drivers/rknpu/rknpu_job.c)：
`task_obj_addr` 用于解析 GEM 对象，任务来自其 `kv_addr`，不是用户指针。
Starry 沿用现有 MemCreate 对象标识，但只在本 open 的 GEM 表中匹配、检查完整任务范围。

持有现有 per-open operation mutex，防止验证与执行之间 MemDestroy 释放 GEM；
导入对象原有 Arc retainer 维持底层分配。逐字节 volatile 快照不建立用户共享存储的 Rust 引用，
随后只解析、校验快照。保留命令缓冲区所属 GEM、32 位 DMA 范围、长度与核心掩码检查。
完成后只回写每个任务的 int_status，避免把旧快照整体覆盖回共享命令。

Direct DMA 中命令流仍可编码任意物理地址；CAP_SYS_RAWIO 持有者是可信原始设备操作者，
本方案不验证全部 NPU 指令，也不为该操作者提供内存隔离。未来若要开放普通用户，
必须完成硬件 IOMMU 与 CPU/GEM 映射支持，不能放宽此授权条件。

超时沿用现有同步提交期限和 reset/recovery；恢复失败进入设备 quarantine。
授权不跨请求缓存。新增 unsafe 边界在合入前须由驱动/内存领域审查，本轮仅本地修改与板测。

## 验证与回退

- 同一新用户程序在修复前 Starry 第一帧失败，修复后须完成真实推理及原门槛流程。
- `apps/starry/aka-rk3588/tests/rknpu-submit-access.c` 通过实际 ioctl 检查无效对象、
  外部 open 对象、越界任务和降权后的继承 fd；不得触发有效硬件任务。
- 通用驱动既有 Direct DMA 无权限提交测试保留；宿主测试只证明软件规则。
- 普通 NPU smoke 必须拒绝 `rknn_run fail`、`inference_yolov8_model fail` 和 submit 错误，
  不能只凭汇总标题判定成功。
- 回退内核改动即可恢复拒绝提交；新用户程序会明确失败。

## 系统调用兼容性对照

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| ioctl / AArch64 29：RKNPU_SUBMIT | [Rockchip 77168c8 rknpu_job.c](https://github.com/rockchip-linux/kernel/blob/77168c8d5ab82399f65a80e9f807b50ba37cf483/drivers/rknpu/rknpu_job.c) | 从 vendor GEM 对象取得任务，提交并返回完成状态 | sys_ioctl → Card1File::ioctl → handle_submit → submit_rawio；per-open 锁保护对象 | 部分正确 | 对齐对象 ABI；Direct DMA 额外要求 CAP_SYS_RAWIO，尚无无特权 IOMMU 路径。板上测试结果见本轮报告。 |

## 具体调用示例

1. root 机器人进程调用 `rknn_run()`，runtime 通过 `RKNPU_SUBMIT` 提交任务。
   旧实现首先检查 translated IOMMU；本板使用 Direct DMA，返回 `EOPNOTSUPP`，
   即使模型已初始化，也不能处理第一帧。修复后，每次提交检查当前进程的
   `CAP_SYS_RAWIO`，授权调用进入显式 `unsafe submit_rawio`；普通入口仍拒绝 Direct DMA。
2. runtime 用 `MemCreate` 得到 `obj_addr`，随后设置
   `submit.task_obj_addr = create.obj_addr`。这个值指向驱动管理的 GEM 对象存储，
   并不是进程 mmap 后得到的用户地址。旧 `vm_load(current, task_obj_addr, ...)`
   按用户地址复制，即使绕过第一处拒绝，仍会失败。现在先在本 open 的 GEM 表验证完整范围，
   在持有操作锁时读取任务快照，校验后提交，完成后仅回写 `int_status`。
3. 进程打开 card1 后执行 `setuid(1000)`，再通过继承的 fd 提交：修复后返回 `EPERM`。
   未知对象、其他 open 的对象和越界任务仍返回 `EFAULT`，不会触发有效硬件任务。

这两个兼容性问题均可在 #2327 的差异中找到；不撤销该提交中已有的对象归属、
DMA 范围、同步恢复和生命周期保护，也不撤销最新 #2424 的 GEM 分配配额。

## NPU 修复阶段的性能基线

下表记录车轮反馈加强前的 NPU 修复验收，基线为
`089f9d8f7433993c3bf023d399c2aa86a6cfa1b8` 加本文的 NPU 修复，
用户程序为 `e157d7c5fb24cad14a74e35482a8070ca93284ab`。两个 guest 保持原来的
单 CPU 0（A55，MPIDR `0x00`）；不保留 A76 绑核绕行。
性能门槛在各 board TOML 中显式传给 `run_robot_ci_once.sh`，允许修改配置，
不接受宿主 `ROBOT_CI_MIN_FPS` 隐式覆盖。

2026-09-17，在 `10.3.10.60:2999` 的 `OrangePi-5-Plus-robot-emmc-01` 实测：

| 环境 | 最新实测 FPS | 配置门槛 FPS | 完整流程 |
| --- | --- | --- | --- |
| AxVisor + Starry，单 CPU0/A55 | 30.01 | 28.0 | PASS，attempts=1 |
| AxVisor + Linux，单 CPU0/A55 | 29.86 | 28.0 | PASS，attempts=2 |
| 原生 Starry，SMP=8 | 30.00 | 28.0 | PASS，attempts=2 |

每项采样为两个约 10 秒性能窗口，真实 UVC + RKNN + Feetech；完整通过还要求
后续控制流程与停车成功。相对于约 30 FPS 的摄像头处理上限，28 FPS 留约 6.7% 波动余量。
原生 Starry 和 Linux guest 首次执行器初始化超时，均通过原有的一次重试完成；
不隐去失败，也未扩展重试次数。
Linux 使用本板已有 6.1.99 guest 镜像和 eMMC 根分区适配；仓库的 SD 配置不改根分区。
日志在工作区 `robot-ci-final-20260917/`，包括每个性能窗口、失败尝试和最终结果。


权限拒绝路径此前已在原生 Starry 和单 A76 guest 实板通过；修复前 guest 在第一项
返回 EOPNOTSUPP 而不是预期 EFAULT，回归程序失败。
历史实板结果保存在工作区 `robot-ci-repair-e157d7c/REPORT.md`，不能当成最新 CPU 0 基线。
机器人 CI 使用独立目录 `/home/orangepi/robot-ci/aka-rk3588`；旧目录
`/home/orangepi/robot/aka-rk3588` 保留给合入前的 CI。部署与固定源码包更新是独立步骤，
新目录部署清单记录实际二进制、脚本及工作区补丁哈希，不将未提交产物标成已发布版本。

## 固定用户态版本与独立目录验收

当前部署包固定用户态提交 `dc95d5502f93b61adb7a7c590a170358b93e840a`，包含三轮双向反馈、
最终零速反馈、命令错误传播和独立目录的 RKNN 运行库加载。源码归档和预编译程序的
SHA256 以 `apps/starry/aka-rk3588/source.env` 为准，打包入口验证两者后生成部署包。
预编译程序与 194 已验收二进制相同（SHA256 `492c81d0ec17aaecf16be338636abe265445a11441787d1247de1a128a4af74e`）。

194 标准板卡用例的原生 Starry、AxVisor + Linux、AxVisor + Starry 分别为
30.01、29.86、29.78 FPS，均首次通过 28 FPS 门槛；原生 Linux 为 30.00 FPS，
按原有一次重试后通过。两个 guest 均为单 CPU0/A55。旧部署目录的 513 个文件
哈希不变，新目录沿用本板校准配置；日志见工作区 `robot-ci-194-20260917/REPORT.md`。
