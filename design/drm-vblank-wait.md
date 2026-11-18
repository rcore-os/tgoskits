# DRM vblank 等待验证契约

## 1. 等待语义

`DRM_IOCTL_WAIT_VBLANK` 等待的是显示序列，不是调用者可观察的固定睡眠时长。
Starry 的 `sys_ioctl()` 经 `KapiFile::ioctl()`、`drm_ioctl()` 进入
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

### 2.3 重新启用

`drm_crtc_vblank_on_config()` 通过 `drm_reset_vblank_timestamp()` 为新模式显式增加
一个序号并建立时间锚。没有引用时，第一次 `CRTC_GET_SEQUENCE` 才可能通过
`drm_crtc_vblank_get()` 启用计时器；无硬件计数器的 `drm_update_vblank_count()`
按时间戳差四舍五入补算活动帧数。固定 Linux 的
[模式切换增量](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L278)
和[时间戳取整](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L339)
共同决定重新启用后的上界。

`vblank_advance_matches_active_time()` 保留序号不能后退的判据，将模式切换之外的
增量与活动时间的最近帧数比较。关闭期间不进入活动时间。若关闭 100 个周期，
重新启用到查询经过 3/4 个周期，合法增量为模式切换的 1 加四舍五入的 1。
原先向下取整的上界会错误拒绝该结果。

既有引用生命周期测试使用每个夹具独立的驱动时间戳，经过真实共享核心的
off/on/get 路径，精确检查增量 2 及其时间戳；系统测试判据还须拒绝把关闭期间
100 个周期补入计数的结果。宿主夹具验证共享核心规则，Starry 定向 QEMU 继续
验证实际 ioctl、计时器、事件边沿与最外层失败传播。

### 2.4 查询与序号起点

查询当前序号使用 `_DRM_VBLANK_RELATIVE` 和零增量；立即事件使用
`DRM_CRTC_SEQUENCE_RELATIVE` 和零增量。绝对零不是这两个操作的通用写法。
旧 `WAIT_VBLANK` 先把 32 位绝对值扩展到当前计数附近，事件到期判定还使用
有限的比较窗口。计数超过窗口或经过 32 位回绕后，绝对零可以成为未来目标，
也可以留在尚未送达的事件队列中。固定 Linux 的
[纯查询条件](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L1685)、
[序号比较](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_internal.h#L109)
和 `drm_crtc_queue_sequence_ioctl()` 决定这些语义。

系统程序的 `query_vblank()` 与 `queue_immediate_sequence()` 统一构造实际请求，
保留事件归属、每个打开描述的容量、部分 readv 和错误后事件留队的原有判据。
`vblank_wait_child()` 通过已有同步管道回传实际等待回复，父进程要求它等于
禁用事件的冻结序号，时间戳只差旧接口的微秒截断，且尚未经过请求的相对增量。
禁用后的 `CRTC_GET_SEQUENCE` 本来返回 EINVAL，不能用它取得冻结快照。
测试不要求绝对序号小于相对增量。受控 C 回放覆盖较小起点、CI 的高计数及 32 位回绕附近，并要求
把目标序号伪装成完成回复时失败；该回放验证测试本身，不代替真实内核运行。

### 2.5 计时器的时间锚

启用过程可以在定时器已设定截止时间后发布一个较晚的时钟采样。任务定时器
尚未前推时，`expiry - interval` 可能早于这个采样。若落后超过半个周期，
`drm_update_vblank_count()` 的无符号时间差和最近整数取整会得到巨量帧数。
该风险属于本地任务定时器与共享核心的接入边界，不能靠系统测试只接受小计数规避。

`drm_crtc_vblank_get_vblank_timeout()` 在 `interval_lock` 下读取周期，并通过
`hrtimer_get_expires()` 取得受定时器队列锁保护的截止时间；校验序号快照后，
返回推导边沿与已发布边沿中的较晚值。锁顺序为 vblank 调用方保护、
`interval_lock`、定时器队列锁，定时器执行器在运行回调前已释放队列锁。
没有更晚边沿时保留已发布的实际采样，不推进计数；后续边沿仍按原规则补算。
这是对任务定时器的适配差异，固定 Linux 的计数算法保持不变。

核心测试 `timer_vblank_preserves_the_published_edge_until_the_next_frame()`
使用真实 hrtimer 存储及真实时间戳 producer，受控建立周期 4 ms、已发布时间
3 ms、截止时间 4 ms 的状态。旧实现把推导的 0 ms 与 3 ms 相减，错误得到
3186109819 个序号；修后保留原边沿，并在截止时间前推后验证真实边沿只计入一次。
夹具在观察前同步取消计时器以固定交错，不能据此声称验证了真实调度时序；
系统程序继续负责实际 Starry 定时器、ioctl、等待和事件读取链路。
