# QEMU 启动组

一个场景目录中的 `build-*.toml` 负责构建，`qemu-*.toml` 负责一次宿主启动，`[axbuild].checks` 按顺序引用 `tests/*.toml`。检查脚本和客户机配置放在所属场景；跨架构 Web 探针在 `common/web-ui`，VMX/SVM 共用的客户机配置在 `x86_64/common/guests`。

## 分组与 CI

| 架构 | 启动组 | 组内检查顺序 | 主 CI |
| --- | --- | --- | --- |
| AArch64 | `general` | `shell-and-block` → `pool` → `ivc` → `pool-http` → `dashboard` | 全组，五项 |
| AArch64 | `http-control-plane` | `browser-console` → `control-plane` | 全组，最小web构建 |
| AArch64 | `memory-web` | `memory-root` → `dashboard` | 全组，无盘Memory根 |
| AArch64 | `pci-memory-test` | ECAM发现、BAR2边界、memory decode与写入读回 | 上游新增专项 |
| AArch64 | `ivc` | Linux/ArceOS 混合 IVC | 未登记；原客户机协议不匹配问题保留 |
| RISC-V64 | `smoke` | `shell-and-block` → `smp-ipi` | 只选择 `shell-and-block` |
| LoongArch64 | `smoke` | `shell-and-block` | 全组 |
| Intel VMX | `smoke` | `shell-and-block` | 全组 |
| Intel VMX | `functional` | `direct-acpi` → `mp-fallback` → `ovmf-acpi` → `pci-enumeration` → `pci-block-rw` → `pci-block-ro` | 全组 |
| Intel VMX | `web-ui` | `dashboard` | 全组 |
| AMD SVM | `smoke` | `shell-and-block` | 全组 |
| AMD SVM | `functional` | `direct-acpi` → `ovmf-acpi` → `pci-enumeration` → `pci-block-rw` → `pci-block-ro` | 全组 |

普通测试共12个启动组，28项功能检查。主 CI 在原25项功能、10次宿主启动基础上，保留新主线加入的 PCI memory 专项，共26项功能、11次宿主启动。夜间 GICv2/GICv3 在 `apps/axvisor`，仍分别启动，不计入此表。SVM 执行需要 AMD KVM；VMX 通过不能替代 SVM 实跑。

## 执行与单项选择

```sh
# 单独诊断 AArch64 shell/block；主 CI 选择下面的完整 general 组。
cargo xtask axvisor test qemu --arch aarch64 \
  --test-case qemu/aarch64/general/shell-and-block

# 五项CI功能共享一次QEMU启动。
cargo xtask axvisor test qemu --arch aarch64 \
  --test-case qemu/aarch64/general

# VMX 六项共享一次 QEMU 启动。
cargo xtask axvisor test qemu --arch x86_64 \
  --test-case qemu/x86_64/vmx/functional

# 单独诊断其中一项；自动执行其必要准备，不执行其他功能检查。
cargo xtask axvisor test qemu --arch x86_64 \
  --test-case qemu/x86_64/vmx/functional/pci-block-ro
```

同一命令选择同组多个成员，仍按 `checks` 声明顺序执行且只启动一次。`requires` 只登记必要准备，不是新增功能；每份准备同一次启动仅执行一次。

CI 作业在 `.github/ci/checks/axvisor.toml` 登记。`command` 与 `check.suite.cases` 同步选择整组或限定成员。增量路由按实际成员及其依赖选择，同组成员合入一条命令，避免拆成多次启动。

## 合批后的状态恢复

- AArch64 `general` 统一4核、8GiB、NVMe和Web UI。块设备客户机VM3/VM4在各自首次退出前sync/umount。
- CLI pool、IVC、HTTP pool及dashboard前按显式requires用API回收运行中的客户机；builtin候选不删除。单项选择只执行所需准备，不执行未选功能。
- builtin VM1/VM2为IVC发布者/订阅者，VM3/VM4为块设备客户机，VM6为dashboard。CLI pool仍使用动态VM5；HTTP pool使用动态VM12/13/14，保留原3客户机和8终端槽耗尽/释放检查。
- 两类pool统一目录`/pool`。HTTP pool先核对已知builtin ID集合，再明确断言动态客户机ID不在builtin中，保证实际从动态池启动。HTTP阶段前清理CLI pool的临时配置并复制原HTTP夹具。
- guest配置直接放general根目录的`guest-*.toml`。`tests/`登记功能与必要准备；`probes/`实现API检查；`sh/`承载安装到客户机文件系统的夹具。
- RISC-V SMP/IPI 前退出块设备客户机、回收 VM1/VM2，再启动具有原3个 vCPU 和亲和性的 VM3。单选 SMP/IPI 时仍使用组的宿主启动配置，自动启动的 VM1/VM2 会先被回收，不执行它们的块设备检查。
- x86 functional 中客户机 ID 唯一，按成员逐个启动、检查、退出并删除。原启动协议、固件、CPU 亲和性、驱动黑名单及只读/读写配置保持；只读项原有的删除和重建步骤也保留。

## 保留独立启动的原因

- VMX/SVM 依赖不同的硬件虚拟化后端，不能合并。
- x86 NVMe 基础组不能与 passthrough functional 顺序合批：直通客户机启动会按内核契约释放宿主磁盘文件系统，后续成员无法再从 builtin 读取资产。functional 保持无盘宿主。
- `pci-memory-test` 保留上游4核、4GiB、无盘宿主及专用ECAM客户机环境；本次变基只迁移路径，不把它并入8GiB磁盘根general或SMP1的memory-web。
- 最小 `web` 的 browser-console 明确要求完整 UI 不存在，不能与 `web-ui` 构建合并。
- `memory-web` 验证 Memory 根，磁盘场景验证 NVMe 根，保留两种启动条件。
- 夜间 GICv2/GICv3 使用不同中断控制器；LoongArch 当前只有一个 normal 组。

新增或修改检查先复用兼容启动；所需 features 取兼容并集。合批必须保留原判定和负载，显式恢复终端、客户机和资源状态，实跑并验证前部、中部、末部失败都不能返回通过。不能仅以配置相近就合并，不能放宽断言来制造通过。

## 探针profile与混合执行

功能编号、目录等初始化差异由声明传给探针，不复制检查逻辑：

```toml
[host_http_probe]
probe_script = "../probes/pool.py"
asset_dir = ".."
request_timeout_secs = 60

[host_http_probe.env]
AXVISOR_POOL_VM_IDS = "12,13,14"
AXVISOR_POOL_DIRECTORY = "/pool"
AXVISOR_POOL_BUILTIN_IDS = "1,2,3,4,6"
```

框架提供的HTTP地址、夹具目录、连接/请求预算优先于自定义env，不修改调用进程环境。混合组支持串口→API→串口→HTTP；全局fail_regex监控贯穿HTTP阶段和退出排空，任一失败中止后续检查。单项预算包含去重后的必要准备，且不超过整组预算。

`requires`按检查文件去重，不表示每次引用都重跑。不同阶段需要重复恢复状态时，使用不同的准备声明引用同一实现；每个功能也必须完成自身资源回收。新增客户机不能占用其他功能或终端耗尽夹具的编号，调整编号时同步更新guest配置、probe env和期望集合。

原interleave/atomic-output及其专属重建准备已迁入[apps/suite控制台场景](../../../apps/suite/axvisor/qemu/aarch64/console/README.md)，两项仍共享一次手动触发的应用启动。general现全部五项均为CI，整组选择不会带入手动检查。
