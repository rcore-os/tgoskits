# Linux 客户机虚拟调频与机器人性能测试

一个 AxVisor 板端启动依次承接客户机虚拟调频检查、模拟机器人验证和 60 秒 UVC＋RKNN benchmark。整合原 `robot-linux/virtcpufreq`、`robot-linux-perf30`、`robot-linux-perf30-measure`；两个旧 perf30 入口的负载和门槛相同，新入口保留一套完整执行，合并其诊断变体。

本场景未加入普通 CI、夜间或 benchmark 自动清单。本次只整理配置并静态核对，未构建或运行；旧 stock Linux 的历史结果见 [HISTORY.md](HISTORY.md)，不能替代本场景验收。

## 文件与执行顺序

```text
robot-linux-perf30/
├── build-aarch64-unknown-none-softfloat.toml
├── board-orangepi-5-plus-robot-linux-perf30.toml
├── guest.toml
├── tests/
│   ├── guest-session.toml            进入客户机与设置目录
│   ├── performance.toml              NPU/DMC准备
│   ├── guest-cpufreq.toml
│   ├── robot-virtual.toml
│   └── uvc-rknn-bench.toml
└── guests/cpu5/guest.toml             手动 A/B 输入，默认不引用
```

板卡配置的 `checks` 是唯一执行顺序：

| 顺序 | 文件 | 内容与通过条件 |
| --- | --- | --- |
| 1 | `tests/guest-cpufreq.toml` | 前置进入 Linux并设置原机器人目录；要求 `scaling_driver=virt-cpufreq`，写入并读回 `performance` governor，读取非零当前频率，最后输出唯一 PASS |
| 2 | `tests/robot-virtual.toml` | 前置原 NPU/DMC performance 配置检查，再执行原 `run_vision_usb_ci_once.sh 29.5`；至少一个 NPU，DMC 缺失允许并明确记录，视觉/FT232 原脚本须 PASS |
| 3 | `tests/uvc-rknn-bench.toml` | 校验并记录原 benchmark 资产 SHA256，执行原 60 秒负载；采集 FPS 的原 29.5 门槛、推理次数>0、解码/推理错误为0 |

一次启动串行执行；前一检查未成功就不进入下一检查。原 900 秒预算保留。模拟机器人验证不驱动车轮或机械臂；60 秒 benchmark 是摄像头采集、解码和 NPU 推理，未增加推理 FPS、耗时或内存阈值。

## 启动配置与资产要求

- `guest.toml` 保留完整 `virtual-cpufreq` 设备声明：`domain=1`、`vcpu_count=1`、`performance_table=[256,512,768,1024]`。客户机 vCPU 固定 CPU4（MPIDR `0x400`、亲和性 `0x10`）；domain 1 对应当前 RK3588 big0 调频域，迁移板型时必须重新核对。
- 必须预先准备 `/guest/linux/orangepi-5-plus-6.1.99-virtcpufreq`，内核包含匹配的虚拟调频驱动、`CONFIG_CPUFREQ_VIRT`、`CONFIG_CPU_FREQ_GOV_PERFORMANCE`。仅修改 TOML 不会生成该内核；缺失内核或驱动直接失败，不回退到 stock Linux。
- 保留原 initramfs、eMMC rootfs、`/home/orangepi/robot-ci/aka-rk3588-virtual` 部署包、摄像头和 FT232 回环条件。
- `/rknn_yolov8_image` 必须包含可执行 `rknn_yolov8_bench`、`lib/librknnrt.so`、`lib/librga.so`、`model/yolov8.rknn` 与 `model/coco_80_labels_list.txt`。构建来源仍是 `benchmarks/starry/orangepi-5-plus-uvc-rknn/build-image-runner.sh`。
- 宿主原 `cpufreq.default_governor=performance` 保留；实际 OPP 选择和温度限制由宿主负责。客户机检查证明前端识别、请求入口及读回可用，不能仅据此断言物理频率发生切换或热控正确。

## 使用

从仓库根目录运行：

```sh
cargo xtask axvisor app board --app-root apps/suite/axvisor \
  --group board --app orangepi-5-plus/robot-linux-perf30
```

可附加 `--server`、`--port` 和 `--board-type` 选择板卡服务。执行前记录 AxVisor/客户机内核版本、资产哈希和板卡身份；执行后保存每个检查结果、采集与推理 FPS、OPP/温度和板卡释放记录。

`guests/cpu5/guest.toml` 保留原 CPU5 A/B 输入并同步虚拟设备声明；它没有额外自动入口，也不会额外启动。需要使用时显式替换构建中的 `vm_configs`，再恢复默认 CPU4 配置。

## 后续增删

新增同环境检查放入 `tests/<用途>.toml`，在 `checks` 中明确排序。检查文件名表达功能；仅启动探针使用 `smoke`。不得增加不执行的占位文件、重复的场景 ID 或第二套默认执行清单。修改虚拟调频检查应保留驱动缺失时失败；修改性能检查应保留原负载、门槛及资产校验。删除成员时同时清理引用，并核对共享资产的所有消费者。

## 手动只运行一项

```sh
cargo xtask axvisor app board --app-root apps/suite/axvisor \
  --group board --app orangepi-5-plus/robot-linux-perf30 \
  --test-case guest-cpufreq
cargo xtask axvisor app board --app-root apps/suite/axvisor \
  --group board --app orangepi-5-plus/robot-linux-perf30 \
  --test-case uvc-rknn-bench
```

默认仍依次执行三项，共一次启动。`guest-session.toml`保留进入客户机和目录设置，`performance.toml`保留原NPU/DMC配置；两者作为`requires`前置按需执行，同启只执行一次。只选benchmark不执行机器人验证或guest-cpufreq功能检查；该场景的专用内核与虚拟设备要求仍保持。任何前置失败均使该次执行失败。
