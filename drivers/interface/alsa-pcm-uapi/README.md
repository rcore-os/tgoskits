# ALSA PCM 二进制接口

本包定义 Linux 64 位小端 PCM 参数、交错帧传输、状态查询与 ALSA control 消息的结构布局，供 StarryOS 音频适配与接口验证使用。

## 1. 协议边界

布局依据是 [Linux v6.6 的 asound.h](https://github.com/torvalds/linux/blob/v6.6/include/uapi/sound/asound.h)。结构体中的整数、位集合和地址都是未验证输入，操作系统适配层必须在修改采集状态前完成校验。

### 1.1 参数与帧

`HwParams` 表达掩码和区间约束，不能将 `HW_REFINE` 当成读取固定参数；`SwParams` 表达等待阈值、自动启动阈值和指针环回边界。`XferI.frames` 和 `result` 使用帧为单位，`buffer` 是用户虚拟地址，不能作为内核指针或 DMA 地址直接访问。

### 1.2 状态同步

`SyncPtr` 显式保留 Linux 联合体的尾部空间，`SyncStatus` 与 `SyncControl` 各占 64 字节。`SYNC_APPL`、`SYNC_AVAIL_MIN` 置位表示从内核取得对应值，清零才表示应用输入值；调用方不能把方向解释反转。

`Info` 用于 PCM 设备查询及 control 节点的 `PCM_INFO`。`Status` 对应 LP64 的运行状态与时间戳结构，时间戳精度由设备后端决定。

### 1.3 增益控制消息

`control` 模块定义声卡信息、控制元素标识、枚举、类型描述与数值结构。`ElemList.ids` 是用户地址；`ElemInfo.value` 与 `ElemValue.value` 是联合体存储，必须先检查元素类型及数量再解释。保留空间采用完整 Linux 布局。

## 2. 使用约束

本包限定 64 位小端布局，ioctl 编码采用 RISC-V64 使用的 asm-generic 形式。`Pod` 派生验证结构无隐式填充；保留字段应在响应构造时清零，协议值应在消费时检查，而不是直接转换为 Rust 枚举。

具体 ioctl 的支持范围由设备后端决定。Linux UAPI 的许可和来源声明保留在源文件及 Cargo 清单中。`apps/starry/sg2002-audio/audio-check.c` 使用 Linux 系统头文件校验结构布局。
