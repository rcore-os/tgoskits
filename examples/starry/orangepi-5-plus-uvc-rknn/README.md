# Orange Pi 5 Plus UVC + RKNN Streaming Example

This case verifies the UVC-to-NPU pipeline on StarryOS:

1. open the UVC camera and continuously capture MJPEG frames;
2. keep only the latest captured frame so capture is not blocked by inference;
3. periodically decode the latest frame and run YOLOv8 with RKNN;
4. print each detection as `YOLO_RESULT`.

It intentionally does not modify `starry-contest/demo/yolov8`. The image runner
under `rknn-yolov8-image/` is a copied and trimmed version of the RKNN YOLOv8
demo with a streaming runner added for direct UVC capture.

The board rootfs must contain:

- `/usr/bin/uvc-fps`
- `/rknn_yolov8_image/rknn_yolov8_image`
- `/rknn_yolov8_image/rknn_yolov8_stream`
- `/rknn_yolov8_image/lib/librknnrt.so`
- `/rknn_yolov8_image/lib/librga.so`
- `/rknn_yolov8_image/model/yolov8.rknn`
- `/rknn_yolov8_image/model/coco_80_labels_list.txt`

Build the image runner:

```bash
examples/starry/orangepi-5-plus-uvc-rknn/build-image-runner.sh
```

Install it into the board Linux rootfs:

```bash
export BOARD_IP=10.3.10.24
rsync -az --delete \
  examples/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/install/rk3588_linux_aarch64/rknn_yolov8_image/ \
  orangepi@${BOARD_IP}:/tmp/rknn_yolov8_image/
ssh orangepi@${BOARD_IP} '
  printf "%s\n" orangepi | sudo -S rm -rf /rknn_yolov8_image &&
  printf "%s\n" orangepi | sudo -S mv /tmp/rknn_yolov8_image /rknn_yolov8_image &&
  printf "%s\n" orangepi | sudo -S chown -R root:root /rknn_yolov8_image &&
  sync
'
```

Linux-side finite smoke test on the board:

```bash
ssh orangepi@${BOARD_IP} '
  cd /rknn_yolov8_image &&
  export LD_LIBRARY_PATH=/rknn_yolov8_image/lib:/usr/local/lib:/usr/lib/aarch64-linux-gnu:$LD_LIBRARY_PATH &&
  printf "%s\n" orangepi | sudo -E -S \
    ./rknn_yolov8_stream --model model/yolov8.rknn --label model/coco_80_labels_list.txt \
      --device 0 --width 320 --height 240 --fps 30 --duration-sec 8 --infer-every 30 --max-inferences 3
'
```

For continuous manual testing, use `--duration-sec 0 --max-inferences 0` and
stop the program with `Ctrl+C`:

```bash
ssh orangepi@${BOARD_IP} '
  cd /rknn_yolov8_image &&
  export LD_LIBRARY_PATH=/rknn_yolov8_image/lib:/usr/local/lib:/usr/lib/aarch64-linux-gnu:$LD_LIBRARY_PATH &&
  printf "%s\n" orangepi | sudo -E -S \
    ./rknn_yolov8_stream --model model/yolov8.rknn --label model/coco_80_labels_list.txt \
      --device 0 --width 320 --height 240 --fps 30 --duration-sec 0 --infer-every 30 --max-inferences 0
'
```

Run the StarryOS board example:

```bash
cargo starry example board -t orangepi-5-plus-uvc-rknn
```

For the current shared board, pass the concrete board lease endpoint:

```bash
cargo starry example board -t orangepi-5-plus-uvc-rknn \
  -b OrangePi-5-Plus-robot \
  --server 10.3.10.62 \
  --port 2999
```

The Starry `init.sh` runs in continuous mode. The board automation still exits
successfully after the third `YOLO_RESULT` line so automated tests do not hang.
