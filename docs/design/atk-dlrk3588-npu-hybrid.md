# ATK-DLRK3588 NPU 混合拓扑

## 目标与边界

比赛 Task 1--3 在同一套 RAM-only AxVisor 拓扑上运行。StarryOS 负责 AI
感知与客户机间通信，Zephyr 负责实时控制；RK3588 NPU 只归 StarryOS 所有：

```text
AxVisor FP-RR
├── StarryOS Guest (priority 89)
│   ├── vCPU0 -> pCPU1: virtio-net、T2N1、CONTROL/STATUS、通信 IRQ
│   ├── vCPU1 -> pCPU2: 图像预处理、RKNN 提交、后处理
│   └── RK3588 NPU: YOLO 张量推理
└── Zephyr Guest (priority 90)
    └── vCPU0 -> pCPU1: 10 ms 周期任务、控制执行、STATUS 回传
```

两个 Linux 职责域是同一个 StarryOS Guest 的两个 vCPU，并非两套 Linux。
NPU 也不能替代计算 vCPU：视频解码、resize/letterbox、颜色转换、RKNN
runtime 调用和 NMS 仍在 CPU 上执行。

## 资源所有权

生成的 StarryOS 配置必须保持：

```toml
cpu_num = 2
phys_cpu_ids = [0x100, 0x200]
host_sched_priority = 89

[devices]
passthrough = [
  { path = "/npu@fdab0000" },
  { path = "/mmc@fe2e0000" },
]
```

生成的 Zephyr 配置必须保持：

```toml
cpu_num = 1
phys_cpu_ids = [0x100]
host_sched_priority = 90
```

NPU 节点带入三个计算核心的 MMIO、SPI 110--112，以及设备树 phandle
引用的 IOMMU、时钟、复位、电源域和 regulator。NPU 不得同时暴露给
AxVisor host 或 Zephyr。eMMC 控制器由 StarryOS 使用其板载根文件系统；实验
程序和数据通过只读 `/proc/initrd` 解包到 tmpfs，不写入持久文件系统。

FP-RR 下，Zephyr 的 priority 90 可以抢占同在 pCPU1 上运行、priority 89 的
StarryOS 通信 vCPU。StarryOS 的 AI 辅助工作位于 pCPU2，避免重型预处理和
后处理直接阻塞 RTOS。RR 变体保留为同拓扑调度基线。

## 可移植构建

从仓库根目录执行：

```bash
scripts/board/build-atk-zephyr-task123-unified.sh tmp/atk-task123-zephyr-unified
```

脚本只生成 host 侧配置、二进制和 FIT，不连接板卡，不调用 fastboot，也不写
eMMC。默认输入均相对于当前仓库解析；其他机器可用环境变量覆盖：

- `STARRY_KERNEL`：带 `rknpu` 与 `virtio-net` 的 StarryOS 内核；
- `STARRY_INITRD`：包含实验 payload 的 cpio；
- `ATK_HOST_DTB`：ATK-DLRK3588 host DTB；
- `ZEPHYR_BASE`：固定提交的 Zephyr 源码目录。

不得在模板、脚本或生成说明中写入开发者的 home、移动硬盘挂载点或预编译
产物绝对路径。生成配置中的二进制路径来自调用者输入和新构建 manifest。
Zephyr 实板计时器频率固定为 RK3588 的 24 MHz；QEMU 测试仍使用各自平台
配置，不能复用实板频率推导。

## 启动与回滚

板卡启动只允许使用 RAM-only `fastboot stage` 工作流。禁止 `fastboot flash`、
`fastboot erase` 或其他持久化写入。回滚只需复位并 stage 旧 FIT；因为 payload
解包到 tmpfs，复位后不会留下实验文件。

## 验收条件

构建后的配置和启动日志至少应证明：

1. StarryOS `cpu_num = 2`，vCPU0 映射 pCPU1、vCPU1 映射 pCPU2；
2. Zephyr vCPU0 映射 pCPU1，且 FP-RR 优先级 90 高于 StarryOS 的 89；
3. `/npu@fdab0000` 及依赖只分配给 StarryOS，RKNN 完成真实张量推理；
4. StarryOS 通信路径和 Zephyr 完成 `CONTROL -> STATUS` 闭环；
5. Task 1 实板统计使用 24 MHz counter，并在相同拓扑下比较 RR 与 FP-RR；
6. FIT、Guest、模型、场景输入和结果文件均记录 SHA-256。

这套架构的声明边界是：NPU/YOLO提高感知与安全决策能力，FP-RR提高 AI
负载下 RTOS 的调度确定性；不宣称操作系统能缩短 NPU 张量计算本身。
