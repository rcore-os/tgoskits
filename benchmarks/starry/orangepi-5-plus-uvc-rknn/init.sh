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
#
# The image directory is selected in this order:
#   1. an explicit `UVC_RKNN_IMAGE_DIR` is used as it is, without a download;
#   2. an explicit `UVC_RKNN_DEPLOY_URL`, the session-file URL of the deploy
#      archive the case's CMake pipeline uploaded, is fetched into a temporary
#      directory;
#   3. otherwise the persistent Linux rootfs directory /guest/npu_demo/ci-v2.
#
# The board network is not guaranteed while the retries below run: the DHCP
# lease can arrive well after boot, later than those attempts. The session-file
# download is therefore only a fast path that must not decide the verdict: a
# failed download logs a fallback marker and continues with the persistent
# directory, which has to carry the complete install tree.
set -u

mode="${UVC_RKNN_MODE:-bench}"
min_confidence="${UVC_RKNN_MIN_CONFIDENCE:-25}"
core_mask="${UVC_RKNN_CORE_MASK:-all}"
expected_images=3
validate_log="${UVC_RKNN_VALIDATE_LOG:-/tmp/uvc-rknn-validate.log}"
bench_log="${UVC_RKNN_BENCH_LOG:-/tmp/uvc-rknn-bench.log}"
persistent_image_dir="${UVC_RKNN_PERSISTENT_IMAGE_DIR:-/guest/npu_demo/ci-v2}"
fetched_image_dir="${UVC_RKNN_FETCHED_IMAGE_DIR:-/tmp/uvc-rknn-image}"
deploy_url="${UVC_RKNN_DEPLOY_URL:-}"
deploy_archive="${UVC_RKNN_DEPLOY_ARCHIVE:-/tmp/uvc-rknn-deploy.tar.gz}"
deploy_attempts="${UVC_RKNN_DEPLOY_ATTEMPTS:-3}"

verdict_failed() {
  printf '\nUVC_RKNN_%s_FAILED reason=%s\n' "$1" "$2"
  exit 1
}

# While the network is still coming up the session endpoint is unreachable, and
# waiting on it would consume the whole board session, so every attempt is bound
# by short connection and total timeouts and the download is retried only a few
# times. A transfer is accepted only once `tar` can read the archive, which also
# rejects a truncated file.
download_deploy_archive() {
  archive="$1"
  attempt=1
  while [ "$attempt" -le "$deploy_attempts" ]; do
    rm -f "$archive"
    if command -v wget >/dev/null 2>&1; then
      wget -T 10 -t 1 -O "$archive" "$deploy_url" || true
    elif command -v curl >/dev/null 2>&1; then
      curl --connect-timeout 5 --max-time 30 -fsSL "$deploy_url" -o "$archive" || true
    else
      printf 'uvc-rknn: no wget or curl available for the deploy archive\n'
      return 1
    fi
    if [ -s "$archive" ] && tar -tzf "$archive" >/dev/null 2>&1; then
      return 0
    fi
    printf 'uvc-rknn: deploy archive attempt=%s/%s unusable\n' \
      "$attempt" "$deploy_attempts"
    attempt=$((attempt + 1))
  done
  return 1
}

# Every selection above ends in a complete install tree, so the same asset check
# runs once afterwards. A missing live runner must not degrade into a
# fixed-image batch pass, which is why all three executables are required.
require_image_assets() {
  for binary in rknn_yolov8_image rknn_yolov8_stream rknn_yolov8_bench; do
    [ -f "$binary" ] || verdict_failed BENCH "asset_missing=$binary"
    [ -x "$binary" ] || verdict_failed BENCH "asset_not_executable=$binary"
  done
  for asset in \
    model/yolov8.rknn \
    model/coco_80_labels_list.txt \
    validation/images.txt \
    validation/expected.txt; do
    [ -s "$asset" ] || verdict_failed BENCH "asset_missing=$asset"
  done
  [ -d lib ] || verdict_failed BENCH library_dir_missing
  library_found=0
  for library in lib/*.so*; do
    [ -e "$library" ] || continue
    library_found=1
    break
  done
  [ "$library_found" = 1 ] || verdict_failed BENCH library_missing
  validation_image_found=0
  for validation_image in validation/*.jpg; do
    [ -e "$validation_image" ] || continue
    validation_image_found=1
    break
  done
  [ "$validation_image_found" = 1 ] || verdict_failed BENCH validation_image_missing
}

if [ -n "${UVC_RKNN_IMAGE_DIR:-}" ]; then
  # An explicitly named directory is the caller's decision, so nothing is
  # downloaded and the tree is used exactly as deployed.
  image_dir="$UVC_RKNN_IMAGE_DIR"
elif [ -n "$deploy_url" ]; then
  image_dir="$fetched_image_dir"
  rm -rf "$image_dir" "$deploy_archive"
  mkdir -p "$image_dir" || verdict_failed BENCH deploy_image_dir
  extracted=0
  if download_deploy_archive "$deploy_archive"; then
    # `--no-same-owner --no-same-permissions` keep the StarryOS filesystem from
    # applying the archived ownership and modes, which it rejects. A tar
    # without those options still retries a plain extraction, and the asset
    # check below rejects a partial one either way.
    if tar --no-same-owner --no-same-permissions -xzf "$deploy_archive" -C "$image_dir" 2>/dev/null; then
      extracted=1
    elif tar -xzf "$deploy_archive" -C "$image_dir" 2>/dev/null; then
      extracted=1
    fi
  fi
  rm -f "$deploy_archive"
  if [ "$extracted" = 1 ]; then
    printf 'uvc-rknn: deploy archive ready at=%s\n' "$image_dir"
  else
    # The network can still be unavailable when the retries end, so an
    # unreachable session endpoint must not be the verdict: the predeployed
    # persistent tree still has to be checked and benchmarked.
    printf 'uvc-rknn: deploy archive url=%s unavailable; fallback=persistent image_dir=%s\n' \
      "$deploy_url" "$persistent_image_dir"
    rm -rf "$image_dir"
    image_dir="$persistent_image_dir"
  fi
else
  image_dir="$persistent_image_dir"
fi

cd "$image_dir" || verdict_failed BENCH image_dir_missing
require_image_assets

# The deployed binaries resolve the vendored runtime next to themselves, which
# the persistent tree and the fetched archive both provide under `lib/`.
LD_LIBRARY_PATH="$PWD/lib:${LD_LIBRARY_PATH:-}"
export LD_LIBRARY_PATH

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

    # The benchmark program exits zero only after capture and inference
    # finished with at least one inference and zero inference errors. Keeping
    # that judgement inside the program keeps the zero-error and
    # non-zero-inference requirement and its failure propagation unchanged;
    # this wrapper only repeats the verdict as one explicit final line, so a
    # failed run can never reach the success marker.
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
    # Split marker tokens so the serial echo cannot match the runner patterns.
    printf '\n%s\n' 'UVC_RKNN_BENCH''_PASSED'
    ;;
  smoke)
    # Bounded smoke: the program reports success only when the run finishes
    # with zero errors and at least one inference; any other ending reports
    # failure.
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
    printf '%s\n' 'UVC_RKNN_STREAM''_BEGIN'
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
