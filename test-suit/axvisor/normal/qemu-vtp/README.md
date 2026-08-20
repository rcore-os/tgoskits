# VTP E2E test case（StarryOS ↔ FreeRTOS 基于 IP 的网络链路）

本用例在 QEMU aarch64 / Axvisor 下同时启动 **StarryOS** 与 **FreeRTOS** 两个
客户机，各挂一块 `virtio-net` 虚拟网卡，经 Axvisor 内部 L2 switch 直连，
构成基于 IP 协议栈的双向链路；两端运行共享码编的 **VTP** 应用层协议
（控制指令 / 状态回传 / 错误通知 / 双向数据）。

## 目录

```text
qemu-vtp/
├── build-aarch64-unknown-none-softfloat.toml   # Axvisor build wrapper（guest VM 列表）
├── qemu-aarch64.toml                            # QEMU 启动 + success/fail 正则
├── run.sh                                       # 一键编排：构建镜像 + 注入 rootfs + 跑用例
├── README.md
└── protocol/                                    # 共享 VTP 码编（两端共用）
    ├── vtp.h / vtp.c
    └── vtp_test.c                               # host 单测
```

## 运行

```bash
cd test-suit/axvisor/normal/qemu-vtp
./run.sh
```

前置条件（容器或 Linux 主机）：

1. **StarryOS guest**：`cargo xtask starry build --arch aarch64` 产出
   `target/aarch64-unknown-none-softfloat/release/starryos.bin`。
2. **Starry VTP server**：`run.sh` 用 aarch64 交叉编译器静态编译
   `test-suit/starryos/qemu/system/axvisor-vtp-server/src/main.c` + `vtp.c`，
   并注入到 Starry rootfs 镜像（`usr/bin/axvisor-vtp-server`）。
3. **FreeRTOS guest**：按 `guests/freertos-vtp/README.md` 集成 virtio-net 驱动
   + lwIP + VTP agent，产出 `guests/freertos-vtp/build/freertos.bin`。

## 通过判定

QEMU 合并输出同时出现 `STARRY_VTP_PASS` 与 `FREERTOS_VTP_PASS`；任一
`*_FAIL`、`panic`、`VCPU_RUNTIME_ERROR` 即失败（`fail_regex` 优先于
`success_regex`）。

## 已知风险（需在真实 Linux 环境迭代）

- **Starry 根文件系统**：Starry 在 QEMU 下从 nvme 挂载 rootfs；Axvisor 侧需把
  host 挂载的 nvme 透传给 VM1（`starry-smp1.toml` 使用 `passthrough` 全量映射，
  保持 PCI 使能）。若透传路径与预期不符，需按实际 host FDT 调整 `disabled`/
  `passthrough` 列表。
- **FreeRTOS virtio-net 驱动**：12 字节 header 语义、DMA 物理地址、SPI 48 固定
  绑定均须与 Axvisor 设备对齐（见 `guests/freertos-vtp/README.md`）。
- **两 Guest 的 console mux**：并发输出时行前缀可能影响 shell 驱动；若 Starry
  侧启动命令不可靠，可改为在 Starry 根文件系统 init 中自动拉起 server。
