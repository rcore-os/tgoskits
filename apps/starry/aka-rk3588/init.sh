app_root=/home/orangepi/robot/aka-rk3588
marker=AKA_RK3588_DEMO

echo "${marker}_BEGIN"

if [ ! -x "$app_root/build/tennis" ] ||
  [ ! -s "$app_root/models/tennis.rknn" ]; then
  echo "${marker}_FAILED reason=linux_deployment_required"
else
  export LD_LIBRARY_PATH="$app_root/lib:$app_root/3rd/rknpu2/Linux/aarch64:${LD_LIBRARY_PATH:-}"
  if cd "$app_root" && ./build/tennis test-yolo models/tennis.rknn 0; then
    echo "${marker}_PASSED"
  else
    echo "${marker}_FAILED reason=vision_pipeline"
  fi
fi
