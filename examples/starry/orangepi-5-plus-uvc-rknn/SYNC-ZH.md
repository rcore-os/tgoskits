# OrangePi 5 Plus UVC + RKNN Demo 同步说明

## 实现功能

当前分支新增了 `examples/starry/orangepi-5-plus-uvc-rknn` 示例，用于在 OrangePi 5 Plus 上验证 StarryOS 下的摄像头采集和 RKNN NPU 推理流程。

该示例实现的流程是：

1. 通过 UVC 摄像头持续采集 MJPEG 图像帧。
2. 采集线程只保留最新帧，避免推理较慢时阻塞摄像头采集。
3. 主循环按间隔取最新帧，解码后送入 RKNN YOLOv8 模型。
4. 推理结果通过串口打印为 `YOLO_INFER` 和 `YOLO_RESULT`。

实现上没有直接修改 `starry-contest/demo/yolov8`，而是复制了一份 RKNN YOLOv8 demo 到当前示例目录下，并新增了流式 UVC 推理入口 `rknn_yolov8_stream`。

## Linux 根文件系统部署

先在开发机上构建并生成要放入板子 Linux 根文件系统的 RKNN demo：

```bash
examples/starry/orangepi-5-plus-uvc-rknn/build-image-runner.sh
```

构建完成后，产物目录为：

```text
examples/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/install/rk3588_linux_aarch64/rknn_yolov8_image/
```

需要把该目录同步到板子 Linux 根文件系统的：

```text
/rknn_yolov8_image
```

部署后，Linux 根文件系统中应包含：

```text
/rknn_yolov8_image/rknn_yolov8_stream
/rknn_yolov8_image/rknn_yolov8_image
/rknn_yolov8_image/lib/librknnrt.so
/rknn_yolov8_image/lib/librga.so
/rknn_yolov8_image/model/yolov8.rknn
/rknn_yolov8_image/model/coco_80_labels_list.txt
```

## Starry Shell 中启动测试

启动 StarryOS 并进入 shell 后，手动执行：

```bash
cd /rknn_yolov8_image
export LD_LIBRARY_PATH=/rknn_yolov8_image/lib:/usr/local/lib:/usr/lib/aarch64-linux-gnu:${LD_LIBRARY_PATH:-}
./rknn_yolov8_stream \
  --model model/yolov8.rknn \
  --label model/coco_80_labels_list.txt \
  --device 0 \
  --width 320 \
  --height 240 \
  --fps 30 \
  --duration-sec 0 \
  --infer-every 1 \
  --max-inferences 0
```

参数含义：

- `--duration-sec 0`：持续运行，不自动退出。
- `--infer-every 1`：每 1 帧取一次最新帧做推理。
- `--max-inferences 0`：不限制推理次数。

停止测试时使用 `Ctrl+C`。

如果只想做有限次数验证，可以改成：

```bash
./rknn_yolov8_stream \
  --model model/yolov8.rknn \
  --label model/coco_80_labels_list.txt \
  --device 0 \
  --width 320 \
  --height 240 \
  --fps 30 \
  --duration-sec 10 \
  --infer-every 30 \
  --max-inferences 3
```

## 测试效果

正常运行时可以看到：

1. RKNN 模型加载成功，打印模型输入输出 tensor 信息。
2. UVC 摄像头开始持续采集，采集帧率约 28 到 32 FPS。
3. 程序周期性执行 NPU 推理，输出 `YOLO_INFER`。
4. 检测到目标时输出 `YOLO_RESULT`，包含类别、置信度和检测框。

典型输出形式：

```text
stream-rknn: streaming started
stream-rknn: capture_fps=30.00 captured=90 inferred=2 dropped_latest=89 mib_s=0.33 elapsed=3.0
YOLO_INFER index=3 frame=90 sequence=90 latency_ms=20.26 detections=5
YOLO_RESULT index=3 det=0 class=person confidence=64.7% box=(115,77,155,153) center=(135,115) size=40x76
```

当前已验证的效果：

- Linux 下 RKNN demo 能正常加载模型并完成 UVC 采集推理。
- StarryOS 下 RKNN 模型可以初始化，UVC 摄像头可以持续采集。
- 修复了流式推理触发逻辑，避免由于只保留最新帧而错过整除帧，导致长时间 `inferred=0` 的问题。

## 注意事项

- `sys_rseq registration is unsupported; returning ENOSYS` 是当前 StarryOS 对 rseq 的兼容性提示，不影响该 demo 运行。
- `attempt to claim already-claimed interface 1` 当前不是致命错误；只要后续出现 `stream-rknn: streaming started`，说明摄像头流已经启动。
- RGA 在 StarryOS 下可能打印打开失败或回退 CPU 处理的日志；当前 demo 仍可继续走 CPU 图像转换和 RKNN 推理。
