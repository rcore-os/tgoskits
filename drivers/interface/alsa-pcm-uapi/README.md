# ALSA PCM UAPI

该 crate 固定 64 位 x86_64 Linux 的 ALSA 用户态 ABI，供 StarryOS 的 PCM 与 card-control 伪文件层复用。它不拥有后端、PCM 打开状态、DMA、任务或文件系统对象。

## 1. 用途

`alsa-pcm-uapi` 把 Linux 用户态与 `hw` PCM 之间的共享数据布局、ioctl 编号和纯参数规则收敛到独立 crate，使 StarryOS 的文件与运行时层不必复制 wire 定义。该 crate 只回答“字节如何解释、候选参数如何收窄”，设备能力、PCM 会话状态和实际后端仍由调用方持有。

### 1.1 原版工具路径

`PcmInfo`、`CtlCardInfo`、`PcmHwParams`、`PcmSwParams`、`PcmStatus`、`PcmSyncPtr`、`PcmSyncPtr64` 和 `PcmXferi` 对应 Linux v6.12 `include/uapi/sound/asound.h` 的 wire 布局。`SNDRV_PCM_IOCTL_*` 与 `SNDRV_CTL_IOCTL_*` 按 x86_64 的 ioctl 编码生成，覆盖未修改的 `alsa-lib` 1.2.13 `hw` PCM 以及 `aplay -l`、`arecord -l` 所需的查询命令。

### 1.2 参数协商

`PcmConstraints` 表示设备能力与本版支持集合的交集，`refine_hw_params()` 只做收窄/相交，并双向推导 period/buffer 的时间、帧数和字节关系；`set_*_near()` 由原版 ALSA 在用户态尝试候选区间，内核不重开应用约束。`accept_hw_params()` 只在 HW_PARAMS 的最终参数全部具体化后返回 `PcmFormat`。

[`PcmSwParams::validate_and_canonicalize()`](src/params/sw.rs) 的签名为 `validate_and_canonicalize(self, &format, canonical_boundary)`，校验 Linux v6.12 的 timestamp mode/type、非零 `avail_min` 和 silence 关系，并返回写回用户态的 canonical 结构。输入结构的 `boundary` 被忽略，输出 boundary 始终来自 `HW_PARAMS` 决定的 `canonical_boundary`。`start_threshold`、`stop_threshold`、`period_step` 和已弃用的 `xfer_align` 不是额外拒绝条件；`MONOTONIC_RAW` 等数值在 UAPI 中合法，是否支持属于 OS/设备 policy。旧 SW_PARAMS protocol 保留已有 `tstamp_type`，不因 canonicalization 改写它。

## 2. 边界

本节界定 ABI 布局与参数策略能承诺的范围。所有用户地址都作为整数传递，固定 profile 只用于首版设备交集；任何调用方都不能因为本 crate 成功解析一个结构，就推导对应设备已经支持该格式、时钟或 ioctl 行为。

### 2.1 不使用 Rust 引用

用户地址字段保持为 `u64` 整数，所有 wire 类型为 `#[repr(C)]`，状态、flags 和参数编号保持整数常量。reserved 与显式对齐填充由调用方以零初始化；`zerocopy` 派生只用于这些可安全解析的整数布局。

### 2.2 固定版本

当前只实现本版 `S16_LE`、48000 Hz、双声道、`RW_INTERLEAVED` 的参数辅助逻辑。格式、速率、声道与 access 的实际接受集合仍由调用方传入的 `PcmConstraints` 与设备能力取交集，不能把固定 profile 当成设备能力公告。`SNDRV_PCM_VERSION` 按 Linux v6.12 头文件广告 `2.0.18`，但实现只覆盖上述子集，不能据此宣称支持 v6.12 的全部新 PCM 行为。

## 3. 依据

本节的固定源码和测试说明 wire 布局、Linux 状态语义及宿主参数算法的来源。它们是实现依据，不替代 StarryOS 真实 guest 回归，也不表示本 crate 单独构成完整 ALSA 驱动。

### 3.1 Linux 与 ALSA

布局基准为 [Linux v6.12 `sound/asound.h`](https://github.com/torvalds/linux/blob/v6.12/include/uapi/sound/asound.h)，用户态调用语义对照 [alsa-lib v1.2.13 `pcm_hw.c`](https://github.com/alsa-project/alsa-lib/blob/v1.2.13/src/pcm/pcm_hw.c)。这些链接是规范依据，不代表本 crate 已在 QEMU 或板卡上验证。

`probe/abi_probe.c` 用选定的 Linux 头文件和编译器打印实际 `sizeof`、`offsetof` 与 ioctl 值。直接使用原始内核 UAPI（而非 `headers_install` 产物）时，需要按内核源码的注解处理：

```text
cc -std=c11 -D__user= '-D__packed=__attribute__((packed))' \
  -I<linux>/include/uapi probe/abi_probe.c -o abi_probe
```

使用已安装的 `sound/asound.h` 时不添加这两个注解宏。两种头文件来源只影响编译预处理，不改变本 crate 的 wire 类型。

### 3.2 StarryOS 设计

接入边界和首版仿真目标见仓库的 `docs/design/starry-virtio-sound.md`；本 crate 只提供共享 ABI 与纯参数逻辑，不创建 `/dev/snd` 节点，也不提交 VirtIO 请求。

### 3.3 宿主测试

`tests/linux_abi.rs` 对照 Linux v6.12 的 wire 尺寸、偏移、版本与 ioctl 值；`tests/params.rs` 覆盖区间/mask 规则、严格 refine、overflow、SW_PARAMS、canonical boundary 与时间取整。该包已加入 `scripts/test/std_crates.csv`。正式结果包括：`cargo xtask clippy --package alsa-pcm-uapi` 通过；原始 Linux v6.12 头文件 C probe 与本 crate 的尺寸、偏移、版本和 ioctl 期望一致；`cargo xtask test --since 7231a5e73cb0e9d705c4e82881cc74877a4ad2b6` 选中本包并通过 2 个 ABI 与 7 个参数集成测试，同次运行的 `axbuild` 483 个测试通过。

`alsa-pcm-uapi` 的正式 clippy 通过。runtime 已改用共享 canonicalization API，私有重复实现已删除；当前集成快照的 `starry-kernel` clippy 为 `73/73` 通过。宿主测试证明 wire 布局和参数算法；当前数据 mmap errno 修正后的无诊断 headless、playback 和 capture-duplex 为 `3/3` 通过。它们不覆盖完整 Linux PCM 能力，未直接验证的 ioctl 参数与故障顺序仍需按兼容表保留缺口。
