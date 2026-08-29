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

# run6f: LD_PRELOAD weston-side probe (recvmsg/sendmsg timestamps) to split
# the wayland sync round-trip latency. Optional — weston runs without it if
# the download fails.
wget -q -O /tmp/libweston_probe_v8.so http://10.0.2.2:8899/libweston_probe_v8.so 2>/dev/null \
    && export LD_PRELOAD=/tmp/libweston_probe_v8.so \
    && echo "[virgl-test] weston 探针已加载" \
    || unset LD_PRELOAD

/usr/bin/weston \
    --backend=drm-backend.so \
    --renderer=gl \
    --config=/root/.config/weston.ini \
    --idle-time=0 \
    --log=/tmp/weston.log &
WESTON_PID=$!
unset LD_PRELOAD

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
grep -iE 'drm|render|gbm|egl|virgl|virtio|dmabuf|dma_buf|fence|import|wayland extension' /tmp/weston.log 2>/dev/null | head -20 || true

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
    # run6c-1: clean FPS baseline (fb-refresh off, no WAYLAND_DEBUG /
    # EGL_LOG_LEVEL pollution). FB_REFRESH_TASK_ENABLED=false is in the
    # kernel build; this measures the xfer(babe) removal's FPS effect
    # vs run6a's 500+ baseline.
    echo "[virgl-test] run6c-1: 干净 FPS 基线 (60s)..."
    timeout 60 glmark2-es2-wayland 2>&1 \
        | grep -aE 'FPS|FrameTime' | tail -25

    # run6d-2: LD_PRELOAD syscall probe — intercept ioctl/poll/ppoll/select/
    # nanosleep/futex/epoll_wait with per-call timestamps, to locate the
    # ~1.5ms/frame guest-side silence (no card0 ioctls) after the per-frame
    # ioctl burst. Logs to /tmp/probe.log + per-5s [sum] lines to stderr.
    # Served from host via QEMU user-net (10.0.2.2:8899).
    echo "[virgl-test] run6f-2: LD_PRELOAD syscall 探针 (40s)..."
    if wget -q -O /tmp/libsyscall_probe_v7.so http://10.0.2.2:8899/libsyscall_probe_v7.so 2>/dev/null; then
        timeout 40 env LD_PRELOAD=/tmp/libsyscall_probe_v7.so glmark2-es2-wayland 2>&1 \
            | grep -aE '\[probe|\[sum\]' | tail -30
        echo "[virgl-test] probe 尾部 12 行:"
        tail -12 /tmp/probe.log 2>/dev/null
        echo "[virgl-test] weston 探针摘要:"
        grep -a '\[weston\]' /tmp/probe_weston.log 2>/dev/null | tail -8
        echo "[virgl-test] weston 探针尾部 6 行:"
        tail -6 /tmp/probe_weston.log 2>/dev/null
        echo "[virgl-test] probe 段完成"
    # run6g: upload the full per-call probe logs to the host (serial drops
    # lines); host listener: nc -l -p 8900 > /tmp/probe_upload.log
    echo "[virgl-test] 上传 probe 文件到 host..."
    (cat /tmp/probe.log; echo "===WESTON==="; cat /tmp/probe_weston.log) \
        | nc 10.0.2.2 8900 2>/dev/null && echo "[virgl-test] 上传完成" \
        || echo "[virgl-test] 上传失败"
    else
        echo "[virgl-test] probe 下载失败（host http server 未起?）"
    fi
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