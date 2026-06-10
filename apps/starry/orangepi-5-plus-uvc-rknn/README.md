# Orange Pi 5 Plus UVC + RKNN Streaming Example

This case verifies the UVC-to-NPU pipeline on StarryOS:

1. open the UVC camera and continuously capture MJPEG frames;
2. keep only the latest captured frame so capture is not blocked by inference;
3. periodically decode the latest frame and run YOLOv8 with RKNN;
4. print each detection as `YOLO_RESULT`;
5. publish annotated frames directly from StarryOS as an HTTP MJPEG stream.

It intentionally does not modify `starry-contest/demo/yolov8`. The image runner
under `rknn-yolov8-image/` is a copied and trimmed version of the RKNN YOLOv8
demo with a streaming runner added for direct UVC capture.

The board rootfs must contain:

- `/usr/bin/uvc-fps`
- `/rknn_yolov8_image/rknn_yolov8_image`
- `/rknn_yolov8_image/rknn_yolov8_stream`
- `/rknn_yolov8_image/rknn_yolov8_bench`
- `/rknn_yolov8_image/lib/librknnrt.so`
- `/rknn_yolov8_image/lib/librga.so`
- `/rknn_yolov8_image/model/yolov8.rknn`
- `/rknn_yolov8_image/model/coco_80_labels_list.txt`

The fixed-image benchmark board config also expects a validation asset set under
`/rknn_yolov8_image/validation/`:

- `images.txt`: list of the three validation image paths, relative to
  `/rknn_yolov8_image`;
- `expected.txt`: committed source asset with the expected detections for those
  images;
- the three user-provided image files referenced by `images.txt`.

`expected.txt` is consumed as a source asset during routine StarryOS board tests.
Regenerating it on Linux is only a maintenance step for intentional changes to
the model, image set, threshold, RKNN runtime, or postprocess behavior; routine
StarryOS tests should not regenerate it.

This tree currently carries the validation list placeholder only. Do not copy
or synthesize replacement image binaries: add the user-provided three-image set
and its matching committed `expected.txt` before expecting the validation-first
benchmark board config to pass.

Build the image runner:

```bash
apps/starry/orangepi-5-plus-uvc-rknn/build-image-runner.sh
```

Install it into the board Linux rootfs:

```bash
export BOARD_IP=10.3.10.24
rsync -az --delete \
  apps/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/install/rk3588_linux_aarch64/rknn_yolov8_image/ \
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
      --device 0 --width 320 --height 240 --fps 30 --duration-sec 8 --infer-every 2 --max-inferences 3 \
      --http-port 8080 --http-fps 15 --jpeg-quality 80
'
```

Linux-side fixed-image validation should pass before the realtime UVC benchmark
when the validation assets are present:

```bash
ssh orangepi@${BOARD_IP} '
  cd /rknn_yolov8_image &&
  export LD_LIBRARY_PATH=/rknn_yolov8_image/lib:/usr/local/lib:/usr/lib/aarch64-linux-gnu:$LD_LIBRARY_PATH &&
  printf "%s\n" orangepi | sudo -E -S \
    ./rknn_yolov8_bench --validate-list validation/images.txt --expected validation/expected.txt \
      --min-confidence 25 --core-mask all --profile
'
```

The validation command must print:

```text
UVC_RKNN_VALIDATE_PASS images=3
```

Linux-side 60-second benchmark smoke can be shortened during setup:

```bash
ssh orangepi@${BOARD_IP} '
  cd /rknn_yolov8_image &&
  export LD_LIBRARY_PATH=/rknn_yolov8_image/lib:/usr/local/lib:/usr/lib/aarch64-linux-gnu:$LD_LIBRARY_PATH &&
  printf "%s\n" orangepi | sudo -E -S \
    ./rknn_yolov8_bench --model model/yolov8.rknn --label model/coco_80_labels_list.txt \
      --device 0 --width 320 --height 240 --fps 30 --duration-sec 8 --infer-every 1 \
      --report-interval-sec 2 --min-confidence 25
'
```

For continuous manual testing with browser preview, use `--duration-sec 0
--max-inferences 0` and stop the program with `Ctrl+C`:

```bash
ssh orangepi@${BOARD_IP} '
  cd /rknn_yolov8_image &&
  export LD_LIBRARY_PATH=/rknn_yolov8_image/lib:/usr/local/lib:/usr/lib/aarch64-linux-gnu:$LD_LIBRARY_PATH &&
  printf "%s\n" orangepi | sudo -E -S \
    ./rknn_yolov8_stream --model model/yolov8.rknn --label model/coco_80_labels_list.txt \
      --device 0 --width 320 --height 240 --fps 30 --duration-sec 0 --infer-every 2 --max-inferences 0 \
      --http-port 8080 --http-fps 15 --jpeg-quality 80
'
```

Open the live annotated stream from another machine:

```text
http://<board-ip>:8080/stream.mjpg
```

Or fetch the latest annotated frame:

```text
http://<board-ip>:8080/snapshot.jpg
```

If the board is only reachable through SSH, forward the port first:

```bash
ssh -L 8080:127.0.0.1:8080 orangepi@${BOARD_IP}
```

Then open `http://127.0.0.1:8080/stream.mjpg` locally.

Run the StarryOS board example:

```bash
cargo xtask starry app board -t orangepi-5-plus-uvc-rknn
```

The default example command uses `board-orangepi-5-plus.toml`, which runs a
bounded smoke test and exits after the success marker is printed. For continuous
manual testing with browser preview, pass the long-run board config explicitly:

```bash
cargo xtask starry app board -t orangepi-5-plus-uvc-rknn \
  --board-config configs/board-orangepi-5-plus-long-run.toml
```

For the local board service, pass the concrete board type:

```bash
cargo xtask starry app board -t orangepi-5-plus-uvc-rknn \
  -b OrangePi-5-Plus
```

Run the StarryOS benchmark example on the local board service:

```bash
cargo xtask starry app board -t orangepi-5-plus-uvc-rknn \
  --board-config configs/board-orangepi-5-plus-bench.toml \
  -b OrangePi-5-Plus
```

If the board is leased through a non-default shared service, add the matching
`--server` and `--port` values to either command.

The benchmark board config first runs fixed-image validation, then starts the
realtime UVC benchmark. The UVC benchmark does not start the HTTP stream. It
runs camera capture and RKNN inference for 60 seconds, then prints one
machine-readable summary line:

```text
UVC_RKNN_VALIDATE_PASS images=3
UVC_RKNN_BENCH_RESULT duration_sec=... captured=... capture_fps=... inferences=... infer_fps=... bytes=... throughput_mib_s=... dropped_latest=... decode_errors=... inference_errors=... decode_ms_avg=... decode_ms_p50=... decode_ms_p95=... infer_ms_avg=... infer_ms_p50=... infer_ms_p95=... detections=... vm_size_kb=... vm_rss_kb=... vm_hwm_kb=... mem_total_kb=... mem_free_kb=... mem_available_kb=...
UVC_RKNN_BENCH_DONE
```

To split the RKNN path into preprocessing, runtime, output, and postprocess
costs, enable profile mode:

```bash
RKNN_BENCH_PROFILE=1 ./rknn_yolov8_bench --duration-sec 8 --report-interval-sec 2
```

Profile mode adds one final summary line without changing the existing result
line:

```text
UVC_RKNN_BENCH_PROFILE_RESULT profile_samples=... perf_run_query_errors=... total_ms_avg=... letterbox_ms_avg=... inputs_set_ms_avg=... run_ms_avg=... outputs_get_ms_avg=... rknn_perf_run_ms_avg=... postprocess_ms_avg=...
```

Use `RKNN_BENCH_PROFILE_FRAMES=1` or `--profile-frames` for one
`RKNN_PROFILE` line per inference.

The same bounded smoke-test command is also stored in
`board-orangepi-5-plus.toml`, so this direct board command runs the default
example as well:

```bash
cargo starry board \
  -c apps/starry/orangepi-5-plus-uvc-rknn/build-aarch64-unknown-none-softfloat.toml \
  --target aarch64-unknown-none-softfloat \
  --board-config apps/starry/orangepi-5-plus-uvc-rknn/board-orangepi-5-plus.toml \
  -b OrangePi-5-Plus-robot \
  --server 10.30.12.60 \
  --port 2999
```

For the direct long-run command, switch `--board-config` to
`apps/starry/orangepi-5-plus-uvc-rknn/configs/board-orangepi-5-plus-long-run.toml`.
Keep the long-run command running while viewing the stream, and stop it with
`Ctrl+C` when done. StarryOS uses DHCP by default; use the `eth0: DHCP acquired
address ...` boot log as `<starry-board-ip>`, then open
`http://<starry-board-ip>:8080/stream.mjpg`.
