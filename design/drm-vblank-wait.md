# DRM vblank 等待验证契约

## 1. 等待语义

`DRM_IOCTL_WAIT_VBLANK` 等待的是显示序列，不是调用者可观察的固定睡眠时长。
Starry 的 `sys_ioctl()` 经 `Card0File::ioctl()`、`drm_ioctl()` 进入
`drm_wait_vblank_ioctl()`；共享核心拥有每个 CRTC 的 vblank 计数和对应边沿时间戳。
分层及定时器精度限制沿用[共享 DRM 核心接入](../docs/design/shared-drm.md)。

### 1.1 目标与回包

对活动 CRTC 的相对请求，`drm_wait_vblank_ioctl()` 在取得 vblank 引用后读取当前计数，
把请求转换为绝对目标。计数到达或越过目标后可以成功返回；CRTC 禁用也会结束等待。
`drm_wait_vblank_reply()` 通过 `drm_vblank_count_and_time()` 读取一致的计数和时间戳，
回包时间戳表示该序列的 vblank 边沿，并向下截断为微秒，不是 ioctl 返回时刻。

本约定对照本地 Linux v7.1 提交
[`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L1712-L1868)
的 `drm_wait_vblank_ioctl()` 和 `drm_wait_vblank_reply()`。这是本次等待语义的固定参考，
不改变共享 DRM 文档声明的整体移植基线。

### 1.2 迟到与调度

软件 vblank 的 `drm_vblank_timer_function()` 先前推定时器截止时间，再处理 vblank。
没有硬件计数器时，`drm_update_vblank_count()` 根据边沿时间差补算经过的帧数，因此迟到
回调可以一次推进多帧；它不要求逐帧重放回调。该流程与
[Linux v7.1 的定时器路径](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L2169-L2319)
一致。等待条件成立后，调用者仍可能等待调度；本地 `hrtimer` 另有 jiffy 取整和任务调度
延迟。不能从请求的相对帧数推导成功调用的耗时上下界。

## 2. 系统回归边界

`test-suit/starryos/qemu/system/drm-test-drm-modeset/src/main.c` 在同一活动模式下验证
相对等待，保留真实 Starry ioctl、软件定时器、等待队列和回包路径，不用宿主替身证明
运行时语义。内核、测试超时和分组失败传播不因成功时延变化而放宽。

### 2.1 序列与边沿

相对等待两个 vblank 前后，以 `DRM_IOCTL_CRTC_GET_SEQUENCE` 取得同一 CRTC 的一致快照。
正常完成的回复至少跨过请求前的两个序列，且不能超出请求后的序列；比较旧接口的 32 位
序列时须处理回绕。在模式未改变的合成 vblank 时钟内，回复边沿应与实际序列差乘以模式
周期相符，只接受旧接口微秒截断造成的误差，而不是按调用者耗时估算帧数。

`CRTC_GET_SEQUENCE` 的纳秒时间戳与 `WAIT_VBLANK` 的微秒时间戳必须区分精度。
返回序列、边沿和取样结果需要共同校验，不能只检查计数前进或 ioctl 返回零。
现有信号、三秒 `EBUSY` 截止时间和 CRTC 禁用唤醒用例继续负责各自的等待终止语义。

### 2.2 观测与失败

调用前后的 `CLOCK_MONOTONIC` 采样只用于诊断等待、调度和日志成本，不构成成功的周期
窗口。采样区间只包围原始 `SYS_ioctl` 调用及必要的结果保存；断言输出在采样之后。
诊断同时记录实际耗时、模式周期、序列和边沿时间，避免后续失败只留下无数据的窗口错误。

验证应区分两种风险：受控的慢诊断输出不能使正确的等待结果失败；真实回包序列或时间戳
错误仍必须使同一个系统用例及最外层任务失败。测试程序非零退出经
`starry-run-system-tests` 和分组失败标记传播到 `cargo xtask starry test qemu`。
