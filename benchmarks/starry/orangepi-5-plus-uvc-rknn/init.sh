#!/bin/sh
# The StarryOS app-board runner merges the selected board config's `shell_cmd`
# before this file, so every workload of this case must live here exactly
# once. A workload left outside this script would run in addition to the
# selected mode: an unconditional stream here used to extend the bounded
# benchmark with a second, never finishing capture run.
#
# The mode is selected by `UVC_RKNN_MODE` in the board config prelude:
#   bench  - fixed-image validation plus the 60 s capture/inference benchmark
#   smoke  - bounded camera smoke run
#   stream - unbounded stream for manual preview
set -u

image_dir="${UVC_RKNN_IMAGE_DIR:-/rknn_yolov8_image}"
mode="${UVC_RKNN_MODE:-bench}"
min_confidence="${UVC_RKNN_MIN_CONFIDENCE:-25}"
core_mask="${UVC_RKNN_CORE_MASK:-all}"
expected_images=3
validate_log="${UVC_RKNN_VALIDATE_LOG:-/tmp/uvc-rknn-validate.log}"
bench_log="${UVC_RKNN_BENCH_LOG:-/tmp/uvc-rknn-bench.log}"

verdict_failed() {
  printf '\nUVC_RKNN_%s_FAILED reason=%s\n' "$1" "$2"
  exit 1
}

cd "$image_dir" || verdict_failed BENCH image_dir_missing

case "$mode" in
  bench)
    rm -f "$validate_log" "$bench_log"

    # Fixed-image validation runs first. The fixed three-image set is part of
    # the benchmark verdict, not just a warning, so a missing marker fails the
    # whole check before the camera benchmark starts.
    if ! ./rknn_yolov8_bench \
      --validate-list validation/images.txt \
      --expected validation/expected.txt \
      --min-confidence "$min_confidence" \
      --core-mask "$core_mask" \
      --profile >"$validate_log" 2>&1; then
      cat "$validate_log"
      verdict_failed BENCH validate_command
    fi
    cat "$validate_log"
    if ! grep -E -q "^UVC_RKNN_VALIDATE_PASS images=${expected_images}[[:space:]]*$" "$validate_log"; then
      verdict_failed BENCH validate_marker
    fi

    # rknn_yolov8_bench prints UVC_RKNN_BENCH_DONE and exits zero only after
    # capture and inference finished with at least one inference and zero
    # inference errors. Keeping that judgement inside the program keeps the
    # zero-error and non-zero-inference requirement and its failure
    # propagation unchanged; this wrapper only repeats the verdict as one
    # explicit final line, so a failed run can never reach the success marker.
    if ! ./rknn_yolov8_bench \
      --width 160 --height 120 --fps 10 --infer-every 1 \
      --duration-sec 60 --report-interval-sec 5 \
      --min-confidence "$min_confidence" \
      --core-mask "$core_mask" \
      --profile >"$bench_log" 2>&1; then
      cat "$bench_log"
      verdict_failed BENCH bench_command
    fi
    cat "$bench_log"
    printf '\n%s\n' UVC_RKNN_BENCH_PASSED
    ;;
  smoke)
    # Bounded smoke: the program prints UVC_RKNN_STREAM_DONE only after a
    # zero-error run with at least one inference, and UVC_RKNN_STREAM_FAILED
    # otherwise.
    if ! ./rknn_yolov8_stream \
      --fps 10 --infer-every 8 --duration-sec 30 --max-inferences 3 \
      --log-every 1 --min-confidence "$min_confidence" \
      --http-port 8080 --http-fps 2 \
      --publish-width 0 --publish-height 0 --jpeg-quality 70; then
      verdict_failed STREAM smoke_command
    fi
    ;;
  stream)
    # Manual preview: announce the marker the runner waits for, then hand the
    # session over to the unbounded stream with the parameters of
    # `configs/board-orangepi-5-plus-long-run.toml`.
    printf '%s\n' UVC_RKNN_STREAM_BEGIN
    exec ./rknn_yolov8_stream \
      --fps 10 --infer-every 8 --log-every 5 \
      --min-confidence "$min_confidence" \
      --http-port 8080 --http-fps 2 \
      --publish-width 0 --publish-height 0 --jpeg-quality 70
    ;;
  *)
    verdict_failed BENCH "unknown_mode=$mode"
    ;;
esac
