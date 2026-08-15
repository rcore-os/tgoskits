# PR #2062: device-relations 消费者与公共抽象审计

## 结论

`KEEP_CONSUMER_LOCAL`。本次审计不支持把独立的
`drivers/interface/device-relations` 作为公共 workspace crate 提交。
该 crate 已从本分支移除，避免形成未被生产路径使用的第二套设备关系
注册表。

## 审计范围与基线

- 分支：`feat/device-relation-registry`
- 审计前提交：`df459040851d9b4fed0694247daacbad3c83672b`
- 本地上游基线：`upstream/dev` 的
  `7786159aced92d984842855a834b8190676a0422`
- 本地差异计数：`upstream/dev...HEAD` 为 `0 1`。

在本次审计中，Windows Git 的远端 fetch 因
`schannel: AcquireCredentialsHandle failed: SEC_E_NO_CREDENTIALS` 未完成。
因此上述基线是本地已存在的 `upstream/dev`，不是对远端当前 HEAD 的
新鲜性声明；变更推送前必须在具备可用 GitHub 凭据的环境重新 fetch 并
核对 PR base。

## 已有的生产关系与身份来源

### rdrive

`rdrive::DeviceId` 是现有平台设备身份：`Descriptor::new()` 分配
`DeviceId(u64)`，FDT probe 将节点映射到该身份，并通过
`PlatformDevice::register()` 将具体接口发布到 rdrive manager。
FDT 节点还保留 `FdtNodeIdentity`（node id 与 path）。

`rdrive::probe::fdt::FdtInfo::phandle_to_device_id()` 与
`rdrive::fdt_phandle_to_device_id()` 已将 firmware phandle 解析到已注册的
设备身份。FDT child provider 的发布路径还验证 parent ownership、disabled
child、重复 capability，并使用 child 自身的 firmware identity。

### ax-driver

`ax-driver::BindingInfo` 的 `BindingIrq::Source` 已在真实 FDT probe 中表示
中断关系：`FdtIrqSpec { controller: rdrive::DeviceId, cells }`。
`binding_info_from_fdt()` 和
`binding_irq_from_named_fdt_interrupt()` 从 FDT 的 interrupt parent 解析该
关系；USB host、display、input、serial 等注册路径消费同一绑定模型。

### 反例：不应上提的关系

StarryOS USBFS 有运行时 USB 枚举和移除，但其 host/device/bus 状态属于
USB 管理器自身的生命周期，不是可与 camera/DMA/NPU/servo/wheel 混合的
平台级通用关系表。将其接到一个新 registry 会复制状态并引入失效同步
责任。

## 候选消费者判定

| 候选 | 是否真实生产路径 | 结果 |
| --- | --- | --- |
| `drivers/ax-driver` 的 FDT binding/probe | 是 | 已有 `rdrive::DeviceId` 与 typed interrupt/resource binding；不需要新 registry。 |
| StarryOS USBFS runtime discovery | 是 | 关系局限于 USB manager；不适合上提为公共平台 API。 |
| 原 `Camera/DmaEngine/Npu/MotionController/Servo/Wheel` 枚举 | 否 | 没有当前 TGOSKits probe、注册、移除或失效路径作为消费者。 |

## 范围修正

移除的 crate 曾使用自定义 `u32 DeviceId` 和静态 `DeviceKind` /
`RelationKind`。它既没有以 rdrive 身份为键，也没有对接 FDT/ACPI/PCI
discovery，且没有 unbind/remove/invalidate 生命周期。因此不能用额外
unit test 将其证明为公共接口。

后续若出现具体消费者，应先在消费者所在 crate 中实现最小关系状态，
直接使用 `rdrive::DeviceId` 和现有 FDT/ACPI 绑定资料；只有至少两个独立
消费者共享相同的创建、查询与失效语义时，才重新评估公共抽取。

## 验证边界

本次变更是删除未接入生产路径的 crate 与 lockfile 条目，不宣称 QEMU、
KVM 或真实板卡通过。提交前应至少运行：

```text
cargo fmt --all -- --check
git diff --check
```

若后续消费者实现落在 `ax-driver` 或 StarryOS，还应运行该消费者的
feature-aware unit/integration test，并把 host test、QEMU、KVM 和实板
证据分开记录。

## 给 Reviewer 的简短回复草案

感谢指出公共 API 与真实消费者之间的边界问题。审计 rdrive、FDT probe、
ax-driver binding 与 USB discovery 后，我们确认原 `device-relations`
crate 使用了平行的身份和关系模型，且没有合适的生产消费者。因此本分支
已移除该公共 crate，而没有以额外 unit tests 或伪消费者维持它。未来如有
具体驱动需求，会先在相应消费者内使用 `rdrive::DeviceId` 和既有
binding/discovery 生命周期实现最小状态；在多个独立消费者证明共享语义前，
不会重新提出通用 registry。
