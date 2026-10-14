# Orange Pi 三客户机 IVC 验证

一次启动 AxVisor，加载 Linux（VM 1）、StarryOS（VM 2）、Zephyr shell（VM 3），按顺序验证三条链路。期间不重启、不重建客户机。

| 链路 | 发布者 | 订阅者 | 通道 | 判定 |
| --- | --- | --- | --- | --- |
| Linux ↔ Zephyr | Linux 1 | Zephyr 3 | 0x100 | 五轮请求及对应回复，两端通过 |
| Linux ↔ StarryOS | Linux 1 | StarryOS 2 | 0x101 | 五轮请求及对应回复，两端通过 |
| Zephyr ↔ StarryOS | Zephyr 3 | StarryOS 2 | 0x100 | 原 demo 批量发送五条请求，逐条校验五条回复，两端通过 |

通道由发布客户机编号和 key 共同标识。不同链路验证序号与完整消息内容，客户端失败或超时必须使外层板卡测例失败。普通客户端校验后关闭端点，Zephyr 的订阅映射保留到板级复位；本测例不证明物理退休或无复位重跑。

客户机镜像及 `ivc-roundtrip` 统一在 `tgosimages` 的 `./build.sh platform orangepi-5-plus all` 流水线中 构建。Linux 使用自身 initramfs 与独立 virtio-blk 的 `rootfs-aarch64-busybox.img`；StarryOS 使用原有 `rootfs-aarch64-orangepi-jammy.img`，由 virtio-blk 提供，用户程序安装在客户机 `/usr/bin`。所有临时部署文件位于板端 `/guest/ivc-test`。

```sh
cargo xtask axvisor test board -g normal -c board-orangepi-5-plus/ivc --board orangepi-5-plus-ivc \
  -b OrangePi-5-Plus-robot-emmc --server http://10.3.10.60:2999
```

应用位于 `apps/axvisor/normal/board-orangepi-5-plus/ivc`，运行器从 apps 目录发现并选择本目录的一份构建配置和板卡配置。本应用整合三条通信链路，供手动板测使用；既有 `test-suit` 用例及其 CI 计划仍保留。

映射事务完成后必须发布恢复后的状态快照，否则运行中的客户机会被控制台误判为 Paused。该修复只在 TGOSKits 核心维护。

StarryOS 的 IVC 通道保留普通流式读写，不启用 `NodeFlags::PACKET`。一次读取可能返回多条消息，`ivc-roundtrip` 按换行符解析并保留未消费内容。VFS 已完成部分读取后遇到后续错误，应返回已读字节数；首条读取失败仍返回原错误。该修复位于 `fs/ax-fs-ng/src/file/handle/backend.rs`，tgosimages 负责构建镜像和用户程序。

StarryOS 控制台由生成设备树的 `stdout-path` 选择，不硬编码 `console=ttyS0`，也不重定向 shell 标准输入输出。runtime 分配的 ttyS 编号不代表设备树存在同编号的 serial 别名。

最后的 `vm list` 检查三个客户机均为 `running`、VCPU 编号为 0、状态为 `Rdy:1`；VCPU 列是编号，不是数量。

普通 Zephyr IVC 命令同步等待完成，默认超时 30000 毫秒，也可传入 `ivc subscriber 1000`。Linux 先启动发布者。Zephyr–StarryOS 链路先确认 Zephyr 已发布通道，再通过 AxVisor 切换控制台启动 StarryOS 对端，回到 Zephyr 收集最终状态；不再执行 `ivc wait`。

在通信验证后，检查无对端时订阅返回 ETIMEDOUT（目标错误码 -116），耗时 1000 至 1999 毫秒；再启动 30 秒订阅并发送 Ctrl+C，要求提前结束且返回 canceled（-140）。随后普通 latency 命令必须通过，证明工作线程恢复。

Zephyr 提示符为 `zephyr:~$ `。中断测量功能可在控制台手动执行 `irq latency`，不参与本通信测例的自动验收。

带直通设备且仍持有 IVC 端点（含已逻辑关闭端点）的客户机，停止、重置和删除请求会在拆除运行资源前明确返回不支持，保持原运行状态；需要板级复位回收。停止 vCPU 或生命周期设备不能单独证明物理 DMA 已停稳。
