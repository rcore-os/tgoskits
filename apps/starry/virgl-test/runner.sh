#!/bin/sh
# StarryOS VirGL 驱动开发 - Guest 侧测试脚本
# 依赖已由 prebuild.sh 预装，直接启动 Weston 并验证 virgl
set +e

export PATH=/usr/bin:/bin:/sbin:/usr/sbin
export XDG_RUNTIME_DIR=/tmp
chmod 0700 /tmp
export LIBSEAT_BACKEND=noop
export WESTON_ALLOW_ROOT=1
export MESA_LOADER_DRIVER_OVERRIDE=virtio_gpu
# GBM fallback: when loader_get_driver_for_fd returns NULL (DRM_BUS_NONE),
# GBM reads this env var to determine the driver name.
# See: https://gitlab.freedesktop.org/mesa/mesa/-/issues/10271
export driver=virtio_gpu
# Limit GL version to 3.3 (no tessellation) to avoid SET_TESS_STATE
# which the host virglrenderer rejects, killing the entire context.
export MESA_GL_VERSION_OVERRIDE=3.3
export MESA_GLSL_VERSION_OVERRIDE=330

green() { printf '\033[32m%s\033[0m\n' "$*"; }
red()   { printf '\033[31m%s\033[0m\n' "$*"; }

# ============================================================
# 1. 确保服务运行
# ============================================================
rc-service seatd start 2>/dev/null || true
rc-service dbus start 2>/dev/null || true

# ============================================================
# 2. 启动 Weston
# ============================================================
echo "[virgl-test] 启动 Weston..."
rm -f /tmp/wayland-*

/usr/bin/weston \
    --backend=drm-backend.so \
    --renderer=gl \
    --config=/root/.config/weston.ini \
    --idle-time=0 \
    --log=/tmp/weston.log &
WESTON_PID=$!

# 等待 Wayland socket
DISP=""
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    sleep 1
    if ! kill -0 "$WESTON_PID" 2>/dev/null; then
        red "[virgl-test] Weston 退出！日志:"
        tail -30 /tmp/weston.log
        break
    fi
    DISP=$(ls /tmp/ 2>/dev/null | grep '^wayland-[0-9]*$' | head -1)
    [ -n "$DISP" ] && break
done

if [ -n "$DISP" ]; then
    green "[virgl-test] Wayland socket 就绪: /tmp/$DISP"
    export WAYLAND_DISPLAY="$DISP"
else
    red "[virgl-test] Weston 未创建 Wayland socket"
fi

# ============================================================
# 3. 验证 GPU / virgl
# ============================================================
echo "[virgl-test] 检查 DRM 设备..."
ls -la /dev/dri/ 2>/dev/null || red "[virgl-test] /dev/dri 不存在"
echo "[virgl-test] renderD128 sysfs 信息:"
cat /sys/class/drm/renderD128/device/vendor 2>/dev/null || echo "  无 sysfs vendor"
cat /sys/class/drm/renderD128/device/device 2>/dev/null || echo "  无 sysfs device"
echo "[virgl-test] renderD128 uevent:"
cat /sys/class/drm/renderD128/uevent 2>/dev/null || echo "  无 uevent"
echo "[virgl-test] renderD128 subsystem:"
readlink /sys/class/drm/renderD128/device/subsystem 2>/dev/null || echo "  无 subsystem symlink"
echo "[virgl-test] virtio_gpu_dri.so 存在性:"
ls -la /usr/lib/dri/virtio_gpu_dri.so 2>/dev/null || ls -la /usr/lib64/dri/virtio_gpu_dri.so 2>/dev/null || echo "  未找到 virtio_gpu_dri.so"
echo "[virgl-test] weston DRM 初始化日志:"
grep -iE 'drm|render|gbm|egl|virgl|virtio' /tmp/weston.log 2>/dev/null | head -15 || true

# GL_RENDERER 检查（es2_info）：无显示时会打印噪音（Error: couldn't open display），
# 暂时注释掉。需要验证 virgl 是否生效时，去掉以下块的行首 "#" 重新启用。
# echo "[virgl-test] 检查 GL_RENDERER..."
# export MESA_LOADER_DRIVER_OVERRIDE=virtio_gpu
# export EGL_LOG_LEVEL=debug
# export MESA_DEBUG=1
# GL_INFO=$(es2_info 2>&1 || true)
# echo "$GL_INFO" > /tmp/es2_info_full.log
# GL_RENDERER=$(echo "$GL_INFO" | grep "GL_RENDERER" || echo "无法获取")
# echo "  $GL_RENDERER"
# echo "[virgl-test] es2_info 完整日志: /tmp/es2_info_full.log"
# echo "[virgl-test] weston 日志: /tmp/weston.log"
# echo "[virgl-test] --- es2_info stderr (关键) ---"
# echo "$GL_INFO" | grep -iE 'error|fail|cannot|unable|EGL|warning|open|device' | head -20
# echo "[virgl-test] ---"
#
# case "$GL_RENDERER" in
#     *virgl*) green "[virgl-test] ✓ virgl 3D 加速生效" ;;
#     *llvmpipe*) red "[virgl-test] ✗ 软件渲染 (llvmpipe)" ;;
#     *softpipe*) red "[virgl-test] ✗ 软件渲染 (softpipe)" ;;
#     *)
#         red "[virgl-test] ✗ GL_RENDERER 为空 — EGL 初始化失败"
#         echo ""
#         echo "[virgl-test] === 诊断信息 ==="
#         echo "[virgl-test] 1) /tmp/es2_info_full.log 完整 stderr:"
#         cat /tmp/es2_info_full.log | grep -viE '^$' | tail -30
#         echo ""
#         echo "[virgl-test] 2) 检查 eglinfo (surfaceless):"
#         EGL_PLATFORM=surfaceless eglinfo 2>&1 | head -20 || true
#         echo ""
#         echo "[virgl-test] 3) 检查 renderD128 ioctl (drm_info):"
#         drm_info /dev/dri/renderD128 2>&1 | head -20 || echo "  drm_info 不可用"
#         echo ""
#         echo "[virgl-test] 4) weston log (最后30行):"
#         tail -30 /tmp/weston.log 2>/dev/null || true
#         echo "[virgl-test] === 诊断结束 ==="
#         ;;
# esac
#
# echo "[virgl-test] GL_VERSION:"
# echo "$GL_INFO" | grep "GL_VERSION" || true
#
# echo "[virgl-test] GL_EXTENSIONS (前5行):"
# echo "$GL_INFO" | grep "GL_EXTENSIONS" | head -5 || true

# ============================================================
# 4. 运行 GL 测试
# ============================================================
export WAYLAND_DISPLAY="$DISP"

if command -v weston-simple-egl >/dev/null 2>&1; then
    echo "[virgl-test] 运行 weston-simple-egl..."
    timeout 5 weston-simple-egl 2>&1 && green "[virgl-test] ✓ weston-simple-egl 运行成功" || echo "[virgl-test] weston-simple-egl 结束"
fi

if command -v glmark2-es2-wayland >/dev/null 2>&1; then
    echo "[virgl-test] 运行 glmark2-es2-wayland..."
    timeout 600 glmark2-es2-wayland 2>&1
elif command -v glmark2-es2 >/dev/null 2>&1; then
    echo "[virgl-test] 运行 glmark2-es2..."
    timeout 600 glmark2-es2 2>&1
fi

# ============================================================
# 5. 测试完成，保持 VM 运行
# ============================================================
green ""
green "=========================================="
green "  virgl-test 完成"
green "  VM 保持运行中，可手动操作"
green "  退出: poweroff"
green "=========================================="
