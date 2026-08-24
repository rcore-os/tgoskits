# Task 1–3 评委验收入口

所有命令都从仓库根目录执行。评委不需要理解内部几十个实验脚本，只使用：

```bash
scripts/competition/task123.sh doctor
scripts/competition/task123.sh --list
scripts/competition/task123.sh build full
scripts/competition/task123.sh suite acceptance
```

`doctor` 只检查和给出安装提示，不会静默安装系统软件。`build full` 可以复用已下载的源码、模型、rootfs 和工具链，但会删除本项目固定输出目录内的 ncnn、Zephyr、StarryOS、AxVisor 编译结果并从当前 checkout 重新生成。运行证据默认写入 `tmp/competition-task123/evidence/`，每个场景保存 commit、日志、pcap、命令和哈希。

## 下载依赖

下载内容可以放在任意目录，通过环境变量传入；不要把个人主目录写进脚本。下面使用仓库内被忽略的 `tmp/competition-task123/downloads/` 作为示例：

```bash
mkdir -p tmp/competition-task123/downloads

git clone https://github.com/Tencent/ncnn.git \
  tmp/competition-task123/downloads/ncnn
git -C tmp/competition-task123/downloads/ncnn checkout \
  946fe3fb14a8dff8c06df763f67be522167b2f00

git clone https://github.com/zephyrproject-rtos/zephyr.git \
  tmp/competition-task123/downloads/zephyr-dccb09599635bdff17633fa7e9dab014b91dce90
git -C tmp/competition-task123/downloads/zephyr-dccb09599635bdff17633fa7e9dab014b91dce90 \
  checkout dccb09599635bdff17633fa7e9dab014b91dce90
```

另外准备：

- pnnx Linux `20260526`，并设置 `PNNX=/path/to/pnnx`；
- YOLO11n ONNX，SHA256 必须是 `634279b40c07c6391472c51ad45b81ebc48706a9a1fe72dd3396322acd0c053b`，设置 `YOLO_ONNX=/path/to/yolo11n.onnx`；
- AArch64 musl 工具链，设置 `CROSS_ROOT=/path/to/aarch64-linux-musl-cross`，或把其 `bin` 加入 `PATH`；
- 如果源码没有放在上述默认位置，设置 `NCNN_SOURCE` 和 `ZEPHYR_BASE`。

可复制的相对路径配置示例：

```bash
export CROSS_ROOT="$PWD/tmp/competition-task123/downloads/aarch64-linux-musl-cross"
export NCNN_SOURCE="$PWD/tmp/competition-task123/downloads/ncnn"
export ZEPHYR_BASE="$PWD/tmp/competition-task123/downloads/zephyr-dccb09599635bdff17633fa7e9dab014b91dce90"
export PNNX="$PWD/tmp/competition-task123/downloads/pnnx-20260526-linux/pnnx"
export YOLO_ONNX="$PWD/tmp/competition-task123/downloads/yolo11n.onnx"
```

## 推荐验收顺序

时间有限时运行：

```bash
scripts/competition/task123.sh build full
scripts/competition/task123.sh suite acceptance
```

`acceptance` 包含 Task 1 调度器 A/B、Task 2/3 正常闭环、链路 blackout 安全恢复，以及 Task 3 模型输出拒绝。全部十个代表场景使用：

```bash
scripts/competition/task123.sh suite full
```

单个失败不会被包装成成功。脚本非零退出，失败现场保留在输出目录。QEMU 使用短的 `/tmp/tgoskits-task123/*.sock` 控制 socket；所有提交内路径均从仓库根目录解析。每次双 Guest 运行会复制一个临时 rootfs，结束后删除，避免超时退出污染后续场景的基础镜像。

## 录屏建议（8–12 分钟）

1. 展示分支与 commit：`git status -sb && git rev-parse HEAD`。
2. 展示入口：`task123.sh --list`，随后运行 `task123.sh doctor`。
3. Task 1：运行 `task1-scheduler-ab`，展示 RR/FP-RR 两组相同负载和最终比较结果。
4. Task 2 正常链路：展示 Linux/StarryOS 发出 CONTROL、RTOS 收到并返回 STATUS/ACK，以及 pcap verifier 的 PASS。
5. Task 2 故障：展示 blackout 开启、两端进入 Safe、链路恢复、控制循环继续。
6. Task 3：突出 `TASK3_MODEL_READY` 中模型哈希、真实 `TASK3_INFER`/`TASK3_DETECTION`，随后展示 CONTROL 到 RTOS STATUS 的闭环。
7. 展示 `task3-model-rejected` 的拒绝与 Safe 行为，证明错误输出不会驱动控制量。
8. 结尾展示 `TASK123_SUITE_PASS`、证据目录、`git-head.txt`、日志和两份 pcap。

正式录屏前可以完成下载和 `build full`，避免把大部分视频浪费在编译上；但正式视频中的运行场景、PASS 标志和证据目录应现场生成，并展示它们对应的 commit。终端字号建议 18–22，窗口只保留命令和关键日志，长编译过程可剪辑但不要剪掉运行开始、故障注入、恢复和最终 PASS。

## RK3588 NPU 混合拓扑（实板）

上面的 `task123.sh` 是可移植 QEMU 验收入口。实板 NPU 场景使用独立入口，
避免在没有板卡时让 `doctor` 或 CI 错误地要求 Rockchip SDK：

```bash
# 组装 fixed/RKNN 的 Starry /proc/initrd 原始 cpio
scripts/task3/build-hybrid-scene-payload.sh --help

# 用相同 StarryOS/Zephyr 拓扑生成 RR 与 FP-RR 两个 RAM-boot FIT
STARRY_INITRD=tmp/hybrid-rknn.cpio \
  scripts/board/build-atk-zephyr-task123-unified.sh tmp/hybrid-board

# 严格采集与分析 30,000 个 10 ms 样本
python3 scripts/test/rt-partition/run-hybrid-latency.py \
  tmp/hybrid-rr-stress.log --samples 30000
python3 scripts/test/rt-partition/analyze-hybrid-latency.py \
  tmp/hybrid-rr-stress.log --samples 30000 \
  --output tmp/hybrid-rr-stress-analysis
```

完整拓扑、外部 RKNN 运行包的边界和 RAM-only 回滚方式见
`docs/design/atk-dlrk3588-npu-hybrid.md`。场景策略和同侧时钟测量方法见
`docs/design/task3-continuous-scene-ab.md`。实板 30k 与 fixed/RKNN 3+3 的
精简量化结果见 `results/atk-dlrk3588-npu-hybrid-20260824/README.md`。
