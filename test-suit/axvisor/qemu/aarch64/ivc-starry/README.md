# AArch64 StarryOS AXIVC ivshmem

本用例通过 `cargo xtask axvisor test qemu` 启动一个无盘 Axvisor 宿主和两个
源码构建的 StarryOS 客户机，验证新的 Starry `axivc-ivshmem` 后端。稳定入口：

```bash
cargo xtask axvisor test qemu --arch aarch64 --test-case qemu/aarch64/ivc-starry
```

## 装配

- 两个客户机使用同一个
  `target/aarch64-unknown-none-softfloat/release/starryos.bin`。
- 两个客户机各有一个 `ivshmem-pci` 虚拟设备，`link_id = 6`，发布者和订阅者
  分别为 peer 0、peer 1，`max_peers = 2`，`common_size = 0`，
  `output_size = 0xc00000`，为 Message V1 后的 10 MiB payload 留出空间。
- 两端客户机命令行都不配置 transport switch。用户态正常打开 `/dev/axivc0`，
  内核从唯一兼容 PCI endpoint 的 peer ID 推导角色；应用不提供 VM id、
  channel key、channel size、BAR 或 BDF。
- AxVisor 在 BAR2 `0x100 + peer * 8` 发布对齐的小端 `u64` peer epoch；
  Starry 在 attach、poll 以及每次 read/write 前后检查该 epoch，变化即终止会话。
- 没有 legacy ivshmem 设备、直通 PCI 设备或 BDF 假设。
- 客户机内核起始地址和装入地址均为 `0x8020_0000`，DTB 位于
  `0x8000_0000`，initramfs 位于 `0x8400_0000`，每个客户机拥有
  `0x8000_0000..0x9000_0000` 的 256 MiB RAM。该布局沿用已经工作的 AArch64
  Linux/ArceOS Axvisor 客户机布局。

`guest-builds.toml` 的 `host_prepare` 在本用例构建 Axvisor 之前：

1. 从当前 checkout 运行
   `cargo xtask starry build --config .../guest-build.toml --smp 1`；
2. 使用同一份 AArch64 freestanding C 源码生成静态 `/init`；
3. 生成只包含该 `/init` 的 newc + gzip initramfs。

StarryOS 已在自身依赖中启用 `ax-runtime/starry-init`，从 initramfs 内存根执行
`rdinit=/init`。客户机构建额外启用 `ax-driver/serial`，把用户态诊断输出接到
虚拟 PL011 控制台。

`/init` 启动后先以 `O_WRONLY` 打开 `/dev/ttyS0`；全部阶段、PASS 和 FAIL 诊断
都写入该 FD。客户机不再依赖 `console=ttyAMA0`、`console=ttyS0` 或全局 HAL
console 身份匹配。

选择自包含 `/init` 是因为本用例不需要 shell，也不需要从共享根盘提取 BusyBox。
这样不会修改共享 initramfs 生成器，也不会依赖宿主 rootfs 内容或 PCI 顺序。

## 用户态协议

`client/ivc-starry.c` 是双方共用的唯一客户端源码，只使用 AArch64 syscall：

- `openat` 正常打开 `/dev/axivc0`，不经过 publish/subscribe identity ioctl；
- 已打开时第二次 `openat` 必须返回 `EBUSY`，channel 生命周期由该打开文件独占；
- `ioctl(0x80284906)` 查询只含 payload 几何和设备 generation 的
  `struct payload_info`，不返回 VM id、key 或 raw BAR offset；
- 每次协议 `ppoll` 使用 30 秒或 60 秒的单次真实阻塞等待，不按短周期轮询；
- `mmap(fd, offset=0)` 只映射本端 payload 为可写；查询结果给出的 peer offset
  只映射对端 payload 为只读。协议页和 reserved 区不可映射；
- 对 peer payload 的初始 `PROT_WRITE` 映射和后续 `mprotect(+W)` 都必须返回
  `EACCES`；两端 payload 的初始 `PROT_EXEC` 和后续 `mprotect(+X)` 也必须返回
  `EACCES`；越界长度及非零 local/未知 peer offset 也必须失败；
- `read`/`write` 仍只传递 48 字节 Message V1 控制帧，帧内携带长度和 checksum；
- cache ioctl 必须返回 `ENOTTY`；
- `close` 失效当前 channel generation 并唤醒对端 `HUP`。

发布者在 peer 尚未出现时，空读和 48 字节写入都必须返回 `EAGAIN`，此时输出
`before-peer` 阶段标记。发布者先在自己的 payload 中写入 10 MiB pattern，
随后阻塞在 `POLLOUT`，收到事件后拒绝 `ERR/HUP/NVAL`，再发送 48 字节描述帧。

订阅者后启动。收到消息后先用 47 字节缓冲区读取，要求返回 `EINVAL` 且不消费
消息；随后完整读取 48 字节，并校验对端只读 payload 的长度和 checksum。
订阅者再写满自己的 10 MiB payload，发送携带反向 checksum 的 48 字节 ACK。

发布者收到 ACK 后读取对端只读 payload 并校验反向 checksum。此 ACK 完成后，
发布者才关闭 channel；旧 VMA 仍存活时再次 open 必须返回 `EBUSY`，随后才拆除
两个 VMA。订阅者等待该 close 失效 generation 所产生的 `HUP`，并确认后续读
返回 `EIO`。观察到后才关闭，并在旧 VMA 存活时验证同一 `EBUSY` 约束。此前
应用不复用 owner payload。

随后 publisher 拆除首个 VMA 并重新打开 `/dev/axivc0`，形成新的 channel
generation，并阻塞等待尚未重新建立会话的 subscriber readiness。subscriber
完成正常关闭后进入固定等待，不再执行任何用户态 close。宿主对新 generation
执行 `vm stop --force 2`；BAR2 peer epoch 改变必须使 publisher 从 poll 得到
`HUP`，后续 read 返回 `EIO`，且不得交付任何旧队列字节。该结果输出独立的
`forced=host-stop` PASS，与前述 graceful close/HUP 证据区分。

## 通过条件

测试先确认发布者到达 `before-peer`，再启动订阅者。订阅者输出
`payload-ready` 后，必须出现新 Starry 后端实际发出的字面标记：

```text
axivc ivshmem MSI-X event
```

随后必须分别看到订阅者和发布者的 `AXIVC_STARRY ... PASS` 标记。任一错误协议
行为、消息内容或事件缺失都会通过 `AXIVC_STARRY FAIL`、步骤超时或 QEMU
失败规则传播到最外层命令。

同一个 QEMU 启动内还必须看到 `vm stop --force 2` 成功，以及 VM 1 的
`AXIVC_STARRY publisher PASS forced=host-stop ...`。该阶段不重新启动客户机，
不增加 QEMU/VM boot 数。

成功标记输出后，两种角色的 `/init` 都进入无定时器的阻塞 `ppoll(NULL, 0, NULL)`
循环，避免 PID 1 正常退出导致客户机 panic。应用级清理完成后，测试宿主分别执行
`vm delete --force 2` 和 `vm delete --force 1`，并要求看到
`✓ VM[n] deleted completely`。FAIL 会先写完整的 `role/stage/errno` 行，再进入
同一停驻路径，不再退出 PID 1。QEMU 失败规则要求该行带有结束换行后才命中。
