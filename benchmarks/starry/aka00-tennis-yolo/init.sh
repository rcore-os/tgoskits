#!/bin/sh
# The StarryOS app-board runner merges the board config prelude before this
# file and treats its console output as the whole benchmark evidence. The
# validator therefore comes from the deploy archive that CI built from this
# checkout, not from the binary preinstalled in the persistent rootfs: an
# outdated preinstalled validator printed per-image timing lines but no
# `AKARS_TENNIS_BENCH_RESULT` summary, which silently turned a stale smoke run
# into a "benchmark" result.
set -u

deploy_url="${AKARS_TENNIS_DEPLOY_URL:-}"
workdir="${AKARS_TENNIS_WORKDIR:-/tmp/akars-tennis-bench}"
archive="${AKARS_TENNIS_ARCHIVE:-/tmp/akars-tennis-deploy.tar}"
log="${AKARS_TENNIS_LOG:-/tmp/akars-tennis-bench.log}"
attempts="${AKARS_TENNIS_DOWNLOAD_ATTEMPTS:-20}"
retry_seconds="${AKARS_TENNIS_DOWNLOAD_RETRY_SECONDS:-2}"
warmup="${AKARS_TENNIS_WARMUP:-1}"
repeat="${AKARS_TENNIS_REPEAT:-5}"

# The fixed validation set is committed with three images, so the benchmark
# aggregation must report exactly `repeat * 3` samples.
expected_images=3
samples=$((repeat * expected_images))
model_rel=model/yolov8n_tennis_v2.cvimodel
images_rel=validation/images.txt
expected_rel=validation/expected.txt

failed() {
  printf '\nSTARRY_AKA00_TENNIS_BENCH_FAILED reason=%s\n' "$1"
  exit 1
}

[ -n "$deploy_url" ] || failed missing_deploy_url
[ "$warmup" -gt 0 ] || failed "invalid_warmup=$warmup"
[ "$repeat" -gt 0 ] || failed "invalid_repeat=$repeat"

rm -rf "$workdir" "$archive"
mkdir -p "$workdir" || failed workdir_create

# The board only gets its address after boot, so retry the download with a
# bound instead of assuming the network is already usable. A download is only
# accepted once the archive is readable as a tar, so a transfer cut off by
# `--max-time` is retried instead of being extracted as a partial file.
attempt=1
ready=0
while [ "$attempt" -le "$attempts" ]; do
  rm -f "$archive"
  if command -v wget >/dev/null 2>&1; then
    wget -T 30 -O "$archive" "$deploy_url" || true
  elif command -v curl >/dev/null 2>&1; then
    curl --connect-timeout 5 --max-time 120 -fsSL "$deploy_url" -o "$archive" || true
  else
    failed wget_or_curl_missing
  fi
  if [ -s "$archive" ] && tar -tf "$archive" >/dev/null 2>&1; then
    ready=1
    break
  fi
  echo "akars-tennis: waiting for session deploy archive attempt=$attempt/$attempts"
  attempt=$((attempt + 1))
  if [ "$attempt" -le "$attempts" ] && [ "$retry_seconds" -gt 0 ]; then
    sleep "$retry_seconds"
  fi
done
[ "$ready" = 1 ] || failed deploy_download

tar -xf "$archive" -C "$workdir" || failed deploy_extract
rm -f "$archive"

[ -x "$workdir/akars-tennis-validator" ] || failed validator_missing
[ -d "$workdir/lib" ] || failed runtime_libraries_missing
[ -s "$workdir/$model_rel" ] || failed model_missing
[ -s "$workdir/$images_rel" ] || failed image_list_missing
[ -s "$workdir/$expected_rel" ] || failed expected_missing

cd "$workdir" || failed workdir_missing
export LD_LIBRARY_PATH="$workdir/lib:${LD_LIBRARY_PATH:-}"

./akars-tennis-validator \
  "$model_rel" \
  "$images_rel" \
  "$expected_rel" \
  --classes 1 \
  --conf 0.5 \
  --iou 0.5 \
  --warmup "$warmup" \
  --repeat "$repeat" >"$log" 2>&1
status=$?
cat "$log"

[ "$status" = 0 ] || failed "validator_status=$status"
grep -E -q "^AKARS_TENNIS_VALIDATE_PASS images=${expected_images}[[:space:]]*$" "$log" ||
  failed validate_marker
# The aggregation is the benchmark result: without it the run proved only the
# single-pass smoke path.
grep -E -q "^AKARS_TENNIS_BENCH_RESULT .*measured_runs=${repeat} images=${expected_images} samples=${samples} " "$log" ||
  failed bench_result_marker

printf '\n%s\n' STARRY_AKA00_TENNIS_BENCH_PASSED
