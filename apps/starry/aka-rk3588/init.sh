url='${sessionFile:aka-rk3588.tar.gz}'
archive=/tmp/aka-rk3588.tar.gz
app_root=/tmp/aka-rk3588
installed_root=/home/orangepi/robot/aka-rk3588

echo AKA_RK3588_DEMO_BEGIN
rm -rf "$app_root" "$archive"

downloaded=0
attempt=1
while [ "$attempt" -le 6 ]; do
  if command -v curl >/dev/null 2>&1; then
    curl --connect-timeout 10 --max-time 60 -fsSL "$url" -o "$archive" && downloaded=1
  elif command -v wget >/dev/null 2>&1; then
    wget -T 60 -O "$archive" "$url" && downloaded=1
  fi
  [ "$downloaded" = "1" ] && break
  rm -f "$archive"
  sleep 3
  attempt=$((attempt + 1))
done

if [ "$downloaded" != "1" ] || ! tar -xzf "$archive" -C /tmp; then
  echo AKA_RK3588_DEMO_FAILED reason=asset_download
else
  # Calibration and tuned poses belong to the physical robot, not the source
  # release. Reuse an existing board setup when one is available.
  for config in lekiwi_calibration.json lekiwi_pick_config.txt; do
    if [ -s "$installed_root/config/$config" ]; then
      cp "$installed_root/config/$config" "$app_root/config/$config"
    fi
  done

  chmod +x "$app_root/build/tennis"
  export LD_LIBRARY_PATH="$app_root/lib:${LD_LIBRARY_PATH:-}"
  if cd "$app_root" && ./build/tennis test-yolo models/tennis.rknn 0; then
    echo AKA_RK3588_DEMO_PASSED
  else
    echo AKA_RK3588_DEMO_FAILED reason=vision_pipeline
  fi
fi
