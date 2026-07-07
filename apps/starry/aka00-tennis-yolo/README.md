# AKA-00 网球 YOLO 固定图片推理测试

这个目录提供 AKA-00/SG2002/CV181x 上的最小用户态 NPU/TPU 推理测试。
它不依赖摄像头输入，而是使用固定图片和固定期望结果验证用户态推理链路是否可用。

测试覆盖的工作包括：

1. 加载 CV181x 格式的 YOLOv8 网球检测模型。
2. 读取固定 JPEG 图片并完成解码、resize、letterbox 和 RGB planar 预处理。
3. 通过 CVI runtime 调用板端 TPU 推理。
4. 对 YOLOv8 输出做后处理和 NMS。
5. 将检测类别、分数和 bbox 与预置 expected 文件比较。
6. 打印每张图片的耗时，用于后续性能观察。

## 目录结构

- `akars-validator/`：Rust 用户态校验程序源码。
- `model/yolov8n_tennis_v2.cvimodel`：CV181x 网球检测模型，来源于
  `BattiestStone4/akars`，提交 `fc15c583849a84126a24660e97e983fbf327ff69`。
- `validation/images.txt`：固定图片清单。
- `validation/*.jpg`：3 张固定测试图片，复用 RK3588 YOLO 测试里的网球图片。
- `validation/expected.txt`：SG2002 Linux 上生成并确认可用的预期检测结果。
- `thirdparty/tpu-sdk-sg200x/`：`scripts/setup.sh` 下载生成的 CVI runtime SDK 目录，仓库不直接保存其二进制内容。
- `scripts/`：Xuantie musl 工具链、TPU SDK 准备和 linker 包装脚本。
- `build-validator.sh`：构建用户态程序，并生成可部署目录。
- `init.sh`：测试入口，假定板端部署路径为 `/akars_tennis`。
- `board-aka-00-sg2002.toml`：后续接入 Starry board test 的配置入口。
- `SHA256SUMS`：模型和图片的哈希记录；工具链和 TPU SDK 压缩包哈希记录在 `scripts/env.sh` 中。

## 构建

第一次构建前准备 Xuantie musl 工具链和 Milk-V/Cvitek SG200x TPU SDK：

```bash
apps/starry/aka00-tennis-yolo/scripts/setup.sh
```

`setup.sh` 会从固定 URL 下载并校验：

- Xuantie V3.4.0 RISC-V musl 工具链。
- `milkv-duo/tpu-sdk-sg200x` 固定提交 `6fa0d80a635db13b6b9dc061d68b8da0593b79f3` 的源码归档。

如果构建环境无法直接联网，可以先准备本地压缩包，然后通过参数指定：

```bash
apps/starry/aka00-tennis-yolo/scripts/setup.sh \
  --toolchain-archive /path/to/Xuantie-900-gcc-linux-6.6.36-musl64-x86_64-V3.4.0-20260323.tar.gz \
  --sdk-archive /path/to/tpu-sdk-sg200x-6fa0d80a635db13b6b9dc061d68b8da0593b79f3.tar.gz
```

如果工具链或 TPU SDK 已存在，也可以通过环境变量指定：

```bash
AKARS_TENNIS_TOOLCHAIN_DIR=/path/to/xuantie-v3.4.0 \
AKARS_TPU_SDK_DIR=/path/to/tpu-sdk-sg200x \
  apps/starry/aka00-tennis-yolo/build-validator.sh
```

常规构建命令：

```bash
apps/starry/aka00-tennis-yolo/build-validator.sh
```

构建产物目录是：

```text
apps/starry/aka00-tennis-yolo/install/sg2002_riscv64_musl/akars_tennis/
```

`install/` 是生成物，已被 `.gitignore` 忽略。需要部署时重新运行
`build-validator.sh` 生成。

## 部署内容

板端部署目录固定为：

```text
/akars_tennis
```

需要把本地构建产物目录中的完整内容部署到板端 `/akars_tennis`：

```text
akars_tennis/
├── akars-tennis-validator
├── run.sh
├── lib/
├── model/
│   └── yolov8n_tennis_v2.cvimodel
└── validation/
    ├── images.txt
    ├── expected.txt
    ├── tennis-ball-black-box.jpg
    ├── tennis-ball-close.jpg
    └── tennis-ball-plant.jpg
```

其中：

- `akars-tennis-validator` 是交叉编译出的 RISC-V Linux 用户态程序。
- `run.sh` 是板端运行入口。
- `lib/` 包含 CVI runtime 以及程序运行需要的动态库。
- `model/` 包含 `.cvimodel`。
- `validation/` 包含固定图片、图片清单和预期结果。

部署方式可以按当前板卡环境选择，例如通过串口辅助、SSH、SCP、rsync、挂载根文件系统等方式完成。核心要求是板端最终存在完整的 `/akars_tennis` 目录，并在写入后执行 `sync`，确保内容落盘。

## 板端运行

在板端 Linux 上执行：

```sh
cd /akars_tennis
./run.sh
```

`run.sh` 会先检查 `/dev/cvi-tpu0`。如果 TPU 设备节点不存在，会尝试加载 rootfs 中的 CV181x TPU 相关模块：

```text
/mnt/system/ko/cv181x_sys.ko
/mnt/system/ko/cv181x_base.ko
/mnt/system/ko/cv181x_tpu.ko
```

随后运行：

```sh
./akars-tennis-validator \
  model/yolov8n_tennis_v2.cvimodel \
  validation/images.txt \
  validation/expected.txt \
  --classes 1 \
  --conf 0.5 \
  --iou 0.5
```

## 结果判定

测试通过时会打印：

```text
AKARS_TENNIS_VALIDATE_PASS images=3
STARRY_AKA00_TENNIS_DETECT_OK
```

每张图片会打印检测结果：

```text
AKARS_TENNIS_RESULT image=0 path=validation/tennis-ball-close.jpg detections=1
AKARS_TENNIS_DET image=0 cls=0 class=tennis_ball score_q10000=9531 confidence_percent=95.31 left=482 top=704 right=776 bottom=1002
```

检测结果字段含义：

- `image`：图片序号，对应 `validation/images.txt` 中的顺序。
- `cls`：模型输出类别 id。当前模型只识别网球，`cls=0` 表示 `tennis_ball`。
- `class`：类别名称，便于直接阅读日志。
- `score_q10000`：模型置信度乘以 10000 后的整数表示，`9531` 表示约 `0.9531`。
- `confidence_percent`：同一置信度的百分比表示，`95.31` 表示 `95.31%`。
- `left/top/right/bottom`：检测框在原图中的像素坐标，分别表示左、上、右、下边界。

每张图片也会打印耗时：

```text
AKARS_TENNIS_TIMING image=0 preprocess_us=... forward_us=... postprocess_us=... total_us=...
```

字段含义：

- `preprocess_us`：CPU 侧预处理耗时，包含 JPEG 解码、resize、letterbox padding 和 RGB planar tensor 打包。
- `forward_us`：TPU 侧模型前向推理耗时，对应 CVI runtime 的 `CVI_NN_Forward` 调用。
- `postprocess_us`：CPU 侧 YOLOv8 后处理耗时，包含输出解析、分数筛选、NMS 和 bbox 坐标还原。
- `total_us`：单张图片端到端耗时，从开始处理图片到得到最终 detection 结果。

其中 `total_us` 包含 `preprocess_us`、`forward_us`、`postprocess_us` 以及少量函数调用和统计开销；性能观察时优先看 `forward_us` 判断 TPU 推理耗时，优先看 `total_us` 判断单张图片完整链路耗时。

## Linux 实测耗时参考

以下数据来自 CI 环境 AKA-00/SG2002 板端 Linux，部署路径为 `/akars_tennis`。每轮都会顺序识别同一组 3 张固定图片。该表用于后续性能对比，CI 是否通过仍以检测结果和成功标记为准。

| 轮次 | 图片 | preprocess_us | forward_us | postprocess_us | total_us |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | 0 | 415022 | 39533 | 1086 | 455648 |
| 1 | 1 | 435312 | 39525 | 1073 | 475917 |
| 1 | 2 | 429350 | 39531 | 1073 | 469962 |
| 2 | 0 | 415454 | 39527 | 1113 | 456101 |
| 2 | 1 | 433679 | 39534 | 1086 | 474306 |
| 2 | 2 | 428220 | 39534 | 1097 | 468858 |
| 3 | 0 | 415529 | 39561 | 1087 | 456184 |
| 3 | 1 | 434228 | 39528 | 1086 | 474849 |
| 3 | 2 | 429114 | 39533 | 1076 | 469729 |
| 4 | 0 | 414924 | 39562 | 1155 | 455649 |
| 4 | 1 | 433084 | 39518 | 1074 | 473682 |
| 4 | 2 | 428085 | 39508 | 1083 | 468683 |
| 5 | 0 | 415256 | 39523 | 1080 | 455865 |
| 5 | 1 | 434598 | 39506 | 1069 | 475180 |
| 5 | 2 | 429529 | 39524 | 1064 | 470124 |

测试失败时会打印：

```text
AKARS_TENNIS_VALIDATE_FAIL reason=...
```

常见失败原因包括模型文件缺失、图片缺失、runtime 动态库缺失、TPU 驱动节点不可用、推理结果与 expected 不匹配。

## 更新 expected

只有模型、图片、runtime 或后处理逻辑变化时才需要重新生成
`validation/expected.txt`。

在板端已部署 `/akars_tennis` 后执行：

```sh
cd /akars_tennis
export LD_LIBRARY_PATH=/akars_tennis/lib:${LD_LIBRARY_PATH:-}
./akars-tennis-validator \
  model/yolov8n_tennis_v2.cvimodel \
  validation/images.txt \
  validation/expected.txt \
  --classes 1 \
  --conf 0.5 \
  --iou 0.5 \
  --write-expected
sync
```

然后把板端生成的 `/akars_tennis/validation/expected.txt` 更新回仓库中的
`apps/starry/aka00-tennis-yolo/validation/expected.txt`，再重新运行
`build-validator.sh` 生成新的部署目录。

## 本地校验

源码层测试：

```bash
cargo test --manifest-path apps/starry/aka00-tennis-yolo/akars-validator/Cargo.toml
```

clippy：

```bash
cargo clippy --manifest-path apps/starry/aka00-tennis-yolo/akars-validator/Cargo.toml
```

固定资产哈希校验：

```bash
cd apps/starry/aka00-tennis-yolo
sha256sum -c SHA256SUMS
```
