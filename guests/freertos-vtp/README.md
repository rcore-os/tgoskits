# FreeRTOS VTP guest（virtio-net + lwIP + VTP agent）

本目录提供 FreeRTOS 客户机一侧的 VTP 演示代码，与 StarryOS 客户机
（`test-suit/starryos/qemu/system/axvisor-vtp-server/`）经 Axvisor 内部
L2 switch 直连，构成基于 IP 的双向网络链路。协议定义见
`docs/design/axvisor-vtp.md`，共享码编在
`test-suit/axvisor/normal/qemu-vtp/protocol/vtp.{h,c}`（本工程经
`TGOSKITS_DIR` 引用，不复制，避免漂移）。

## 组成

| 文件 | 职责 |
| --- | --- |
| `src/virtio_mmio.{h,c}` | VirtIO MMIO 传输层：寄存器、特性协商、状态位、队列配置、notify、config 空间 |
| `src/virtio_net.{h,c}` | split-ring virtio-net 设备：RX/TX 队列、MAC、12 字节 header 语义 |
| `src/vt_platform.h` | 平台钩子：内存屏障（AArch64 DMB-ISH）与 virt→phys 地址转换 |
| `src/lwip_netif.{h,c}` | lwIP netif 桥接：`vt_netif_add()` / `vt_netif_poll()` |
| `src/vtp_agent.c` | VTP agent 任务：应答 REQ_STATUS、双向 DATA、错误通知 |
| `Makefile` | 编译 `libvtp.a`（需外部 FreeRTOS/lwIP 头文件） |

## 关键语义（务必与 Axvisor 设备对齐）

1. **MMIO 基址/IRQ**：`0x0a00_0000` / SPI 48（Axvisor 的 `virtio-net`
   DeviceModel 固定绑定）。若 FreeRTOS 启动时不读运行时 DTB，请把这组值
   编入板级配置；若读运行时 DTB，则无需配置（DTB 已含 `/virtio_mmio@a000000`）。
2. **12 字节 header**：协商 `VIRTIO_F_VERSION_1` 后，Axvisor 设备侧使用
   **12 字节** `virtio_net_hdr`（10 字节基础 + `num_buffers`），与仓库现代
   `virtio-drivers` 客户 ABI 一致（见 `axvirtio-net/src/constants.rs`）。
   TX 必须前置 12 字节零 header，RX 需剥离 12 字节。
3. **DMA 可见性**：描述符表、avail/used ring、数据缓冲必须位于客户机物理
   内存且连续。`vt_platform.h` 的 `vtm_dma_addr()` 默认恒等映射；若 stage-1
   非恒等，请重写该函数。
4. **特性协商**：仅接受 `VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC |
   VIRTIO_NET_F_STATUS`（与 `AXVIRTIO_NET_FEATURES` 一致）。不协商
   event idx / indirect desc / offload。

## 集成步骤（放入已有 FreeRTOS + lwIP AArch64 工程）

1. **FreeRTOS 内核 + 平台移植**：QEMU Cortex-A53（如
   `qemu_cortex_a53` 风格）启动向量、EL1 初始化、GIC、Generic Timer、
   PL011 串口 retarget（`printf` 用于打印 marker）。参考
   `os/axvisor/configs/vms/qemu/aarch64/freertos-smp1.dts` 中的平台形态。
2. **lwIP**：启用 `LWIP_SOCKET=1`、`LWIP_NETCONN=1`、`LWIP_ARP=1`、
   `LWIP_ETHERNET=1`、`LWIP_IPV4=1`、`LWIP_TIMERS=1`（`sys_now()`）。
3. **启动接线**：
   ```c
   #include "virtio_net.h"
   #include "lwip_netif.h"
   #include "vtp_agent.h"   /* extern void vtp_agent_task(void *); */

   void vtp_init(void) {
       virtio_net_t *dev = vt_netif_dev();
       if (virtio_net_init(dev, 0x0a00_0000) != VIRTIO_NET_OK) {
           printf("FREERTOS_VTP_FAIL virtio-net init\n");
           return;
       }
       vt_netif_add();                 /* 10.0.2.16/24, default gw 0 */
       /* 周期任务调用 vt_netif_poll() 排空 RX 队列 */
       /* 本任务运行 VTP agent */
       xTaskCreate(vtp_agent_task, "vtp", 4096, NULL, 3, NULL);
   }
   ```
4. **构建**：`make TGOSKITS_DIR=/path/to/tgoskits "FREERTOS_INC=..." "LWIP_INC=..."`，
   产出 `build/libvtp.a`，与 FreeRTOS/lwIP 静态链接成 `freertos.bin`
   （`kernel_load_addr=0x4000_0000`，入口见 VM 配置）。

## VM 配置 / DTB

- `os/axvisor/configs/vms/qemu/aarch64/freertos-smp1.toml`：已加入
  `[[devices.virtual]] model="virtio-net"`（MAC `52:54:00:12:34:57`）。
- `os/axvisor/configs/vms/qemu/aarch64/freertos-smp1.dts`：参考文档，标注
  `virtio_mmio@a000000`（reg `0x0a00_0000 0x200`、SPI 48、level）。

## CI 验证：freertos.bin 产物要求

双 VM VTP 网络通信已接入 CI：`.github/ci/checks/axvisor.toml` 的
`test-axvisor-aarch64-qemu-vtp` job。它先检查
**`guests/freertos-vtp/build/freertos.bin`** 是否存在，缺失则报清晰错误退出
（见 `test-suit/axvisor/normal/qemu-vtp/run.sh`）。因此把链接出的镜像放到：

```
guests/freertos-vtp/build/freertos.bin
```

必须满足（对齐 `freertos-vtp-smp1.toml` 与 Axvisor virtio-net 设备）：

| 项 | 值 |
| --- | --- |
| 加载基址 | `0x4000_0000` |
| 入口 | `0x4000_1000` |
| virtio-net MMIO / IRQ | `0x0a00_0000` / SPI 48 |
| 静态 IP | `10.0.2.16/24`（与 Starry 侧 VTP server 配对） |
| 成功 / 失败标记 | 打印 `FREERTOS_VTP_PASS` / `FREERTOS_VTP_FAIL` |

准备就绪后 CI 会自动执行：
1. 构建 StarryOS guest（`cargo xtask starry build`）。
2. 编译 VTP server 并注入 Starry rootfs。
3. 以 `starry-smp1.toml` + `freertos-vtp-smp1.toml` 双 VM 启动 Axvisor QEMU
   aarch64，两个 guest 经内部 L2 switch 互发 VTP，直到两边都打印 PASS。

本地手动验证：`test-suit/axvisor/normal/qemu-vtp/run.sh`。

## 端到端

见 `test-suit/axvisor/normal/qemu-vtp/` 与 `apps` 侧的运行脚本。
StarryOS 为 controller（`STARRY_VTP_PASS`），本 agent 为
responder（`FREERTOS_VTP_PASS`）。
