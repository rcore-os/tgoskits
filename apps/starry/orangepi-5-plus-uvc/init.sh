url='${sessionFile:usr/bin/uvc-fps}'
program=/tmp/uvc-fps
download="${program}.part"
download_attempts=12
download_retry_seconds=5

rm -f "$program" "$download"
attempt=1
while [ "$attempt" -le "$download_attempts" ]; do
  rm -f "$download"
  if command -v curl >/dev/null 2>&1; then
    curl --connect-timeout 10 --max-time 30 -fsSL "$url" -o "$download" || true
  else
    wget -T 30 -O "$download" "$url" || true
  fi
  if [ -s "$download" ]; then
    break
  fi
  echo "uvc-fps: waiting for session asset attempt=$attempt/$download_attempts"
  attempt=$((attempt + 1))
  sleep "$download_retry_seconds"
done
if ! [ -s "$download" ]; then
  printf '\n%s%s\n' 'UVC_SESSION_ASSET_' 'FAILED'
  exit 1
fi
mv "$download" "$program" && chmod +x "$program" || {
  printf '\n%s%s\n' 'UVC_SESSION_ASSET_' 'FAILED'
  exit 1
}

rm -rf /root/uvc-frames
mkdir -p /root/uvc-frames
sleep 5
"$program" --device 0 --format mjpeg --auto-min-data --interval-sec 1 --duration-sec 3 --restart-rounds 3 --save-dir /root/uvc-frames --save-last --max-saved 1
sync
ls -l /root/uvc-frames
set -- /root/uvc-frames/frame-*.jpg
if [ "$#" -ne 1 ] || [ ! -s "$1" ]; then
  printf '\n%s%s\n' 'UVC_FRAME_VALIDATION_' 'FAILED'
  exit 1
fi
printf '\n%s%s\n' 'UVC_ENDPOINT_' 'LIFECYCLE_OK'
