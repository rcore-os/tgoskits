#!/bin/sh
# StarryOS virgl 人工验收脚本（在 guest 内运行）。
#
# 本脚本不做“通过/失败”自动判定：它启动 Weston 和一个可见的 weston-terminal
# 窗口，打印 DRM/Mesa/EGL 诊断信息，可选地用 strace 跟踪一次 glmark2（内核跟踪
# 能力不完整时自动退化为直接运行 glmark2），然后一直休眠，等待操作者关闭 QEMU
# （QMP quit、界面 poweroff 或宿主机中断 cargo xtask）。
#
# 有意不输出 VIRGL_TEST_PASSED / VIRGL_TEST_FAILED 之类机器判定标记，任何命令的
# 退出码都不参与判定，脚本也不会用 `exit` 结束自己或结束 QEMU。
#
# 由 /etc/profile.d/virgl-autostart.sh 在 root 交互登录时后台启动：stdout/stderr
# 只写入 /tmp/virgl-runner.log，不再向 /dev/console 写第二份输出。
#
# 必须兼容 BusyBox/Alpine 的 /bin/sh（ash）：不使用 bash 专属语法。
set -u

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

WESTON_LOG=/tmp/weston.log
GLMARK_LOG=/tmp/glmark2.log
GLMARK_STRACE=/tmp/glmark2.strace
GLMARK_STRACE_LOG=/tmp/glmark2.strace-attempt.log
TERMINAL_LOG=/tmp/weston-terminal.log

MANUAL_BLOCKERS=0
DIAGNOSTIC_WARNINGS=0

say() {
    printf '[virgl-test] %s\n' "$*"
    return 0
}

blocker() {
    printf '[virgl-test][BLOCKER] %s\n' "$*"
    MANUAL_BLOCKERS=$((MANUAL_BLOCKERS + 1))
    return 0
}

note() {
    printf '[virgl-test][DIAG] %s\n' "$*"
    DIAGNOSTIC_WARNINGS=$((DIAGNOSTIC_WARNINGS + 1))
    return 0
}

tail_file() {
    if [ -f "$1" ]; then
        say "$2 ($1):"
        tail -n "${3:-40}" "$1" 2>/dev/null || true
    fi
    return 0
}

log_hits() {
    # Diagnostic only: hits are printed for humans, never used to stop the VM.
    # grep 的退出码在这里只表示“有没有命中”，不参与任何判定。
    pattern="$1"
    shift
    hits=$(grep -iE "$pattern" "$@" 2>/dev/null || true)
    if [ -n "$hits" ]; then
        note "日志命中疑似 GPU/GL 错误（仅供参考，不影响 VM 生命周期）:"
        printf '%s\n' "$hits" | head -n 20
    else
        say "日志未命中可疑错误模式: $pattern"
    fi
    return 0
}

diagnose() {
    tail_file "$WESTON_LOG" "weston 日志尾部" 40
    tail_file "$GLMARK_LOG" "glmark2 日志尾部" 60
    tail_file "$TERMINAL_LOG" "weston-terminal 日志尾部" 20
    say "串口日志文件: /tmp/virgl-runner.log（画面: VNC 127.0.0.1:5909）"
    return 0
}

stop_pid() {
    pid="$1"
    [ -n "$pid" ] || return 0
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    return 0
}

keep_vm_alive() {
    # 保持 VM 运行供人工检查。本脚本由登录脚本在后台启动，不会被登录 shell 等待；
    # 只有操作者关闭 QEMU（poweroff / QMP quit / 宿主机中断）才会结束整个 app。
    # 这里有意不 `exit`，任何前面的命令结果都不会让 runner 或 QEMU 结束。
    while :; do
        sleep 3600
    done
}

# 收到信号时只停掉本次启动的子进程，不结束 runner 自身：VM 交给操作者关闭。
on_signal() {
    say "收到退出/中断信号，停止本次启动的子进程（VM 仍保持运行，等待人工关闭）..."
    stop_pid "${TERMINAL_PID:-}"
    stop_pid "${WESTON_PID:-}"
    return 0
}
trap on_signal INT TERM HUP

# ============================================================
# 1. 启动 Weston（drm-backend + GL renderer）
# ============================================================
# 注意：guest 内没有 OpenRC/rc-service，也不在运行时安装软件包（rootfs 不保留
# apk 数据库）。所有依赖（含 strace）都在 prebuild 阶段一次性打进 rootfs。
say "启动 Weston (drm-backend.so, renderer=gl)..."
rm -f /tmp/wayland-*

/usr/bin/weston \
    --backend=drm-backend.so \
    --renderer=gl \
    --config=/root/.config/weston.ini \
    --idle-time=0 \
    --log="$WESTON_LOG" &
WESTON_PID=$!

DISP=""
i=0
while [ "$i" -lt 15 ]; do
    sleep 1
    i=$((i + 1))
    if ! kill -0 "$WESTON_PID" 2>/dev/null; then
        break
    fi
    DISP=$(ls /tmp/ 2>/dev/null | grep '^wayland-[0-9]*$' | head -n 1)
    if [ -n "$DISP" ]; then
        break
    fi
done

if [ -n "$DISP" ]; then
    say "Wayland socket 就绪: /tmp/$DISP"
    export WAYLAND_DISPLAY="$DISP"
else
    blocker "Weston 没有创建 Wayland socket（进程可能已退出，见下面的 weston 日志）"
fi

# ============================================================
# 2. DRM / Mesa / virgl 诊断
# ============================================================
say "检查 DRM 设备..."
if [ -e /dev/dri/card0 ]; then
    say "存在 /dev/dri/card0"
else
    blocker "/dev/dri/card0 缺失，guest 没有 DRM 显示设备"
fi
if [ -e /dev/dri/renderD128 ]; then
    say "存在 /dev/dri/renderD128"
else
    blocker "/dev/dri/renderD128 缺失，guest 没有 render node"
fi
ls -la /dev/dri/ 2>/dev/null || say "/dev/dri 不存在"

say "renderD128 sysfs 信息:"
cat /sys/class/drm/renderD128/device/vendor 2>/dev/null || say "  无 sysfs vendor"
cat /sys/class/drm/renderD128/device/device 2>/dev/null || say "  无 sysfs device"
say "renderD128 uevent:"
cat /sys/class/drm/renderD128/uevent 2>/dev/null || say "  无 uevent"
say "renderD128 subsystem:"
readlink /sys/class/drm/renderD128/device/subsystem 2>/dev/null \
    || say "  无 subsystem symlink"

say "virtio_gpu_dri.so 存在性:"
if ls -l /usr/lib/dri/virtio_gpu_dri.so 2>/dev/null \
    || ls -l /usr/lib64/dri/virtio_gpu_dri.so 2>/dev/null; then
    :
else
    blocker "virtio_gpu_dri.so 缺失，Mesa 无法加载 virtio_gpu gallium 驱动"
fi

say "weston DRM/EGL 初始化日志:"
grep -iE 'drm|render|gbm|egl|virgl|virtio' "$WESTON_LOG" 2>/dev/null | head -n 15 \
    || say "  weston 日志尚无相关条目"

if [ -n "$DISP" ]; then
    if command -v es2_info >/dev/null 2>&1; then
        say "es2_info 报告（人工核对 GL_RENDERER 是否为 virgl）:"
        es2_info 2>&1 | grep -iE 'GL_RENDERER|GL_VENDOR|GL_VERSION|EGL_VERSION' || true
    fi
    if command -v eglinfo >/dev/null 2>&1; then
        say "eglinfo GBM 段（截取）:"
        eglinfo -B 2>&1 | head -n 25 || true
    fi
fi

# ============================================================
# 3. 用 weston-terminal 产生人工可见窗口
# ============================================================
# 系统里只有 weston-terminal（没有 weston-simple-egl/shm/touch），因此可见窗口
# 由 weston-terminal 提供；它不参与任何判定。
TERMINAL_PID=""
if command -v weston-terminal >/dev/null 2>&1 && [ -n "${WAYLAND_DISPLAY:-}" ]; then
    say "启动 weston-terminal（人工可见的 Weston 窗口）..."
    weston-terminal --maximized >"$TERMINAL_LOG" 2>&1 &
    TERMINAL_PID=$!
    sleep 3
    if kill -0 "$TERMINAL_PID" 2>/dev/null; then
        say "weston-terminal 正在运行 (pid=$TERMINAL_PID)"
    else
        blocker "weston-terminal 启动后立即退出，人工画面上不会有窗口"
        tail_file "$TERMINAL_LOG" "weston-terminal 日志" 40
        TERMINAL_PID=""
    fi
else
    blocker "找不到 weston-terminal，或 Wayland socket 不可用（人工画面窗口不可用）"
fi

# ============================================================
# 4. glmark2 + strace（诊断输出，不参与 QEMU 生命周期判定）
# ============================================================
GLMARK=""
if command -v glmark2-es2-wayland >/dev/null 2>&1; then
    GLMARK=glmark2-es2-wayland
elif command -v glmark2-es2 >/dev/null 2>&1; then
    GLMARK=glmark2-es2
elif command -v glmark2 >/dev/null 2>&1; then
    GLMARK=glmark2
fi

if [ -n "$GLMARK" ] && [ -n "${WAYLAND_DISPLAY:-}" ]; then
    # strace 只是可选诊断：StarryOS 内核跟踪能力不完整时，strace 可能只打印
    # `strace: exec: Function not implemented` 而根本没有启动 glmark2。因此这里
    # 先尝试跟踪，一旦确认跟踪没有真正跑起目标程序，就明确记录诊断并在同一次
    # runner 流程里直接运行 $GLMARK，保证人工验收能看到真实 renderer/score 和动画。
    # 不用管道：直接重定向到文件，保留 strace 的退出码。
    strace_useful=0
    if command -v strace >/dev/null 2>&1; then
        say "尝试用 strace -ff -tt -yy -s 128 跟踪 $GLMARK 的 ioctl,mmap,munmap,openat,close"
        say "  轨迹文件（跟踪可用时才存在）: ${GLMARK_STRACE}*"
        strace -ff -tt -yy -s 128 \
            -e trace=ioctl,mmap,munmap,openat,close \
            -o "$GLMARK_STRACE" \
            "$GLMARK" >"$GLMARK_STRACE_LOG" 2>&1
        strace_rc=$?
        # 判定“跟踪没有真正启动目标程序”：
        #   * strace 自述了 exec/ptrace 失败（如 exec: Function not implemented）；
        #   * 或者输出里根本没有 GL_RENDERER（说明目标程序没跑起来）。
        if grep -Eq 'strace: .*(exec|Function not implemented|ptrace|Operation not permitted)' \
            "$GLMARK_STRACE_LOG" 2>/dev/null; then
            strace_useful=0
        elif ! grep -qi 'GL_RENDERER' "$GLMARK_STRACE_LOG" 2>/dev/null; then
            strace_useful=0
        else
            strace_useful=1
        fi
        if [ "$strace_useful" -eq 1 ]; then
            say "$GLMARK 已在 strace 下运行完成（退出码 $strace_rc，仅供参考）"
            cat "$GLMARK_STRACE_LOG" >>"$GLMARK_LOG"
            cat "$GLMARK_STRACE_LOG"
        else
            say "strace 未能真正启动 $GLMARK（strace 退出码 $strace_rc，日志无 GL_RENDERER）"
            say "  这通常是 StarryOS 内核跟踪能力不完整，不是 glmark2/virgl 失败。"
            say "  strace 自述信息（截取）:"
            grep -m5 '^strace' "$GLMARK_STRACE_LOG" 2>/dev/null || true
            say "  → 退化为直接运行 $GLMARK，以获得真实 renderer/score 和可见动画"
        fi
    else
        note "strace 不可用（prebuild 未安装 strace？），跳过跟踪"
    fi

    if [ "$strace_useful" -eq 0 ]; then
        # 不用 timeout：由 glmark2 自然结束，或由人关闭 VM。输出同时进 runner log
        # 与 $GLMARK_LOG。退出码只做人工诊断，不参与任何判定。
        "$GLMARK" >>"$GLMARK_LOG" 2>&1
        glmark_rc=$?
        cat "$GLMARK_LOG"
        say "$GLMARK 退出码: $glmark_rc（仅人工诊断，不影响 VM 生命周期）"
    fi
    say "$GLMARK 结束（退出码不决定 QEMU 是否继续运行）"
    renderer=$(grep -i -m1 'GL_RENDERER' "$GLMARK_LOG" 2>/dev/null || true)
    if [ -n "$renderer" ]; then
        say "glmark2 报告: $renderer"
        case "$renderer" in
            *virgl*)
                say "  → 这条报告显示走的是 virgl 硬件加速路径" ;;
            *)
                note "  → 这条报告不含 virgl，可能是软件渲染回退（请人工确认）" ;;
        esac
    else
        note "glmark2 日志里没有 GL_RENDERER 行，请人工查看 $GLMARK_LOG"
    fi
    score=$(grep -i -m1 'glmark2 Score' "$GLMARK_LOG" 2>/dev/null || true)
    if [ -n "$score" ]; then
        say "glmark2 分数: $score（仅供参考）"
    else
        note "glmark2 没有输出 Score 行，请人工查看 $GLMARK_LOG"
    fi
    log_hits 'submit.*(error|fail)|context.*(error|lost)|illegal resource' \
        "$GLMARK_LOG" "$WESTON_LOG"
else
    note "glmark2 或 Wayland socket 不可用，跳过基准诊断（不影响人工验收）"
fi

# ============================================================
# 5. 人工验收提示
# ============================================================
say "=========================================="
say " virgl-test 人工验收阶段（没有自动终止条件）"
say "=========================================="
say "宿主机普通 VNC 客户端直连: 127.0.0.1:5909 （display 9，raw VNC/RFB，不是 HTTP）"
say "浏览器访问: 先在宿主机运行 noVNC/websockify 把 6080 代理到 5909，"
say "            再打开 http://127.0.0.1:6080/vnc.html?autoconnect=1"
say "SPICE 备选: spicy --uri=\"spice+unix:///tmp/starry-virgl-test.sock\""
say "串口仍可交互: 本脚本输出只写入 /tmp/virgl-runner.log，串口控制台照常可用"
say "关键日志: $WESTON_LOG, $GLMARK_LOG, $TERMINAL_LOG"
say "glmark2 strace（可选诊断，内核不支持时自动退化为直接运行，轨迹可能不存在）:"
say "  ${GLMARK_STRACE}* ；strace 尝试输出: $GLMARK_STRACE_LOG"
say "人工检查项:"
say "  1. VNC 画面能看到 Weston 桌面和 weston-terminal 窗口（画面只是可视证据）"
say "  2. 上面的 glmark2 GL_RENDERER 行显示 virgl（而不是 llvmpipe/softpipe）"
say "  3. 串口与 $WESTON_LOG 没有持续刷 GPU 提交错误"
say "关闭 VM: guest 内 poweroff，或宿主机用 QMP 依次发送 qmp_capabilities 与 quit:"
say "  printf '{\"execute\":\"qmp_capabilities\"}\\n{\"execute\":\"quit\"}\\n' | socat - UNIX-CONNECT:/tmp/starry-virgl-test.qmp"
say "  （QMP 是有状态协议，只发 quit 会被拒绝；也可按 README 的 python3 QMP 步骤关闭）"
if [ "$MANUAL_BLOCKERS" -gt 0 ]; then
    say "注意: 本次有 $MANUAL_BLOCKERS 项关键准备未就绪（上面标记 [BLOCKER]），"
    say "      VM 会继续运行以便人工排查，请先看这些日志再决定。"
fi
if [ "$DIAGNOSTIC_WARNINGS" -gt 0 ]; then
    say "另有 $DIAGNOSTIC_WARNINGS 条诊断提醒（标记 [DIAG]），仅供参考。"
fi
say "接下来不自动判定、不自动退出，直到操作者关闭 QEMU。"
diagnose

# ============================================================
# 6. 保持运行，等待人工关闭
# ============================================================
keep_vm_alive
