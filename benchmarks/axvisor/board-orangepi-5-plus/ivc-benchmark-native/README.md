# Orange Pi 5 Plus 原生 Starry/Zephyr IVC benchmark

该板卡用例通过原生 `ivshmem-pci` 端点运行真实的 Zephyr producer 和
StarryOS consumer。两台客户机都从 AxVisor host initramfs 启动，不会写入
板卡持久 Linux 根文件系统。

producer 必须先输出 `AXIVC Zephyr benchmark producer pass`。最终成功门槛是
StarryOS 输出：

```text
AXVISOR_IVC_BENCH_RESULT=PASS cases=4 testTime=100 bytes=1232076800 chunks=400
```

## 缓存模式

[`prepare-assets.sh`](./prepare-assets.sh) 用 `AXIVC_BENCH_BAR2_CACHE`
选择 ivshmem BAR2 的缓存策略。变量未设置时保持现有的 `non-cacheable`
行为。显式可选值只有
`non-cacheable` 和 `write-back`，其他值（包括空字符串）会在读取 artifact
或开始构建前直接失败。脚本从同一个已校验变量生成两份 guest 配置，因此
Starry 和 Zephyr peer 的 `bar2_cache` 不会出现不一致。

当前生产路径由管理员为两个匹配的 AArch64 managed peer 显式设置
`bar2_cache = "write-back"`，并加入现有 artifact 启动路径：

```sh
AXIVC_BENCH_BAR2_CACHE=write-back \
AXIVC_BENCH_ARTIFACT_DIR=/path/to/ivc-stage \
  cargo xtask axvisor test board \
  --board orangepi-5-plus-ivc-benchmark-native \
  --board-type OrangePi-5-Plus
```

也可以由本地 tgosimages checkout 同源构建 guest 并在一次板卡命令中完成
write-back prepared 验证：

```sh
AXIVC_BENCH_PREPARED=1 \
AXIVC_BENCH_BAR2_CACHE=write-back \
TGOSIMAGES_DIR=/path/to/tgosimages \
  cargo xtask axvisor test board \
  --board orangepi-5-plus-ivc-benchmark-native \
  --board-type OrangePi-5-Plus
```

`prepare-assets.sh` 调用 builder 时会让 Starry 与 Zephyr 自然继承这两个应用
构建变量。Starry 侧由 [`Makefile`](../../../../apps/starry/ivshmem/Makefile#L13)
把 `AXIVC_BENCH_PREPARED` 和 `AXIVC_BENCH_ITERATIONS` 传给编译，Zephyr 侧由
[`CMakeLists.txt`](../../../../apps/zephyr/ivshmem/samples/benchmark/CMakeLists.txt#L5)
执行相同约束。使用预构建 artifact 时，两端必须由同一个
`AXIVC_BENCH_PREPARED=1` 构建。关闭或省略该变量会恢复严格生成模式。

AxVisor 从
[`IvshmemPciOptions`](../../../../virtualization/axvm/src/configured/devices/ivshmem_pci.rs#L130)
解析该字段。同一 link 的两个 reservation 必须携带相同策略。write-back 下
BAR2 经 host RAM、stage-2、Starry 内核及用户映射和 Zephyr 映射使用
Normal Inner Shareable Write-Back，BAR0/BAR1 仍为 Device 语义。相关客户机
映射分别由 [`axivc_ivshmem.rs`](../../../../os/StarryOS/kernel/src/pseudofs/dev/axivc_ivshmem.rs#L186)
和 [`tgos_ivc_zephyr.c`](../../../../apps/zephyr/ivshmem/src/tgos_ivc_zephyr.c#L106)
维护。

write-back 正式严格匹配 PCI revision 2 和 transport protocol `0x4001`。
原有 non-cacheable 路径继续严格匹配 revision 1 和 `0x4000`。身份映射由
[`bar2_transport()`](../../../../virtualization/axvm/src/configured/devices/ivshmem_pci.rs#L73)
生成，并由 [`policy_for_profile()`](../../../../drivers/ax-driver/src/pci/ivshmem.rs#L318)
双向校验。Message V1 的帧格式和应用接口不变。策略不通过 guest bootarg、
init 脚本或应用开关传递，因此 Starry 的现有 cmdline 和 Zephyr 启动方式都
无需增加参数。

## 镜像配对

write-back 要求 Starry、Zephyr 及其设备后端来自同一源码状态并打包为配套
artifact，且两者都实现 revision 2 与 `0x4001` 的严格组合。
`AXIVC_BENCH_ARTIFACT_DIR`
中的 `SHA256SUMS` 必须覆盖下面列出的整套输入。不能把一个模式的新 guest
镜像与另一个模式或旧源码构建的镜像混用。使用 `TGOSIMAGES_DIR` 时，
tgosimages checkout 也必须从同一工作区构建这组配套产物。

当前信任边界仅覆盖管理员配置为一致共享 RAM 的 RK3588 CPU-only 双 peer
环境：`max_peers = 2`、`common_size = 0`，共享 RAM 不得存在 DMA alias。
仓库不会从任意板卡自动推导该硬件前提，管理员负责只部署经过验证的
Starry/Zephyr 组合。旧 Linux UIO 路径和 ArceOS 适配器只识别原来的
revision 1 与 `0x4000` transport，不支持 revision 2 与 `0x4001`
write-back endpoint，因此不能作为此模式的 peer。

当前已验证的生产组合是 Orange Pi 5 Plus RK3588、StarryOS + Zephyr、两个
peer、`common_size = 0` 且共享 RAM 无 DMA alias。write-back 不改变现有
4 个尺寸、每个 100 次、共 400 次传输和 1232076800 字节的判定。成功仍以
上面的 `AXVISOR_IVC_BENCH_RESULT=PASS` 行为准，吞吐由运行日志中的
`AXIVC_IVC_BENCH_TIMING` 或 `AXIVC_IVC_PREPARED_TIMING` 分段记录计算。

## 输入

用例使用 tgosimages `ivc-rk3588.sh` 的产物。可以直接传入已经 staging 的目录：

```sh
AXIVC_BENCH_ARTIFACT_DIR=/path/to/ivc-stage \
  cargo xtask axvisor test board \
  --board orangepi-5-plus-ivc-benchmark-native \
  --board-type OrangePi-5-Plus
```

也可以传入包含 builder 的本地 tgosimages checkout：

```sh
TGOSIMAGES_DIR=/path/to/tgosimages \
  cargo xtask axvisor test board \
  --board orangepi-5-plus-ivc-benchmark-native \
  --board-type OrangePi-5-Plus
```

artifact 目录必须包含 `SHA256SUMS` 和以下 benchmark 产物：

```text
guest/starry/orangepi-5-plus
guest/zephyr/zephyr-ivc-benchmark.bin
guest/zephyr/zephyr-ivc-benchmark.elf
guest/zephyr/zephyr-ivc-benchmark.dtb
usr/bin/ivc-starry-bench
```

`prepare-assets.sh` 会校验 checksum，从同一份 Zephyr ELF 读取入口点和首个
LOAD 地址，并要求物理地址与虚拟地址一致。它还会从 Zephyr runtime template
删除编译期 `/pci@b000000` 节点，因为 AxVisor 会依据解析后的 runtime device
graph 安装 PCI bridge。

宿主构建环境还需要 `clang`、`rust-lld`、`readelf`，以及
device-tree-compiler 提供的 `fdtget` 和 `fdtput`。

VM 配置路径继续使用日常的 workspace 内 `target/axbuild/...` 逻辑路径。
axbuild 在解析 VM 配置时会把该前缀重映射到实际 Cargo target 目录。当前配置
变量只提供 `workspace`、`package` 和 `tmpDir`，没有原生 target 变量，因此
这里不新增等价占位符。

该用例由 `cargo xtask axvisor test board` 发现并使用上述命令运行。
复测 write-back 时必须重新生成或选择同一源码状态的配套 artifact，不能沿用
non-cacheable 或旧 revision 镜像。接收完整校验、BAR2 搬运和端到端吞吐分别
以 `verify_payload()`、BAR2 分段时间和端到端累计时间为准。prepared 模式的
发送和接收计时分别由
[`main.c`](../../../../apps/zephyr/ivshmem/samples/benchmark/src/main.c#L36)
与 [`ivc_starry_bench.c`](../../../../apps/starry/ivshmem/src/ivc_starry_bench.c#L172)
输出。其批量写不包含 payload 生成，不能当作严格生成路径的含生成吞吐。
