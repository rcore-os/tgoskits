#!/bin/sh
# On-target runner: set up the software desktop-OpenGL runtime and run the native OpenGL compute
# carpets. Prints "TEST PASSED" only when every built carpet reports its "<name> OK <n>" marker.
set -u
BIN=/opt/gpu-opengl
mkdir -p /tmp/glrt
export XDG_RUNTIME_DIR=/tmp/glrt
export LD_LIBRARY_PATH=/usr/lib
# surfaceless EGL: create a desktop-GL 4.3 context with no window-system surface, over the gallium
# llvmpipe DRI driver (CPU software rendering, no GPU).
export EGL_PLATFORM=surfaceless
export LIBGL_ALWAYS_SOFTWARE=1
export GALLIUM_DRIVER=llvmpipe
# StarryOS runs one vCPU (SMP off by default), so llvmpipe's LLVM JIT executes every workgroup on one
# thread. Pin the mesa thread pool to 1 to make that explicit. The carpets assert numerical
# correctness against closed-form references, not throughput, so thread count does not affect results.
export LP_NUM_THREADS=1
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "gpu-opengl: detected CPU count = $ncpu; llvmpipe pinned single-threaded (LP_NUM_THREADS=1)"

pass=0; total=0; fail=0
# run <name> <binary> - a carpet whose binary is absent (did not build on this arch) is skipped.
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "gpu-opengl: $name absent (not built this arch) - skipped"; return 0; }
    total=$((total + 1))
    out="$(cd "$BIN" && "$prog" 2>&1)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        echo "$out" | grep -E ": PASS=|OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -6
        echo "CARPET FAILED: $name (exit $rc)"
        fail=$((fail + 1))
    fi
}

cd "$BIN" || exit 1
# The native desktop-GL compute carpet over the surfaceless-EGL + llvmpipe path (context create /
# make-current / GL 4.3 compute-shader compile+link incl. error paths / SSBO create-map-bind-unmap /
# uniform / glDispatchCompute + glMemoryBarrier / glDispatchComputeIndirect / fence sync / timer
# query / glGetBufferSubData readback / copy-sub-data / clear-buffer-data / program-resource
# reflection / GL_INVALID_* error paths / zero-size + 1M-element boundary dispatch). Every operator
# result is checked element-wise against a closed-form reference.
run opengl_c_egl "$BIN/opengl_c_egl"
# OSMesa desktop-GL carpets: present only if the arch provisioned mesa-osmesa (Alpine ships none on
# any arch today, so these are normally absent and skipped).
run opengl_c     "$BIN/opengl_c"
run opengl_cpp   "$BIN/opengl_cpp"

echo "gpu-opengl: $pass/$total carpets OK on $(uname -m)"
# The surfaceless-EGL desktop-GL compute carpet (opengl_c_egl) is buildable + runnable on every arch
# (mesa-gl + mesa-egl + mesa-dri-gallium exist on all four Alpine arches). The OSMesa carpets are
# additive: Alpine ships no mesa-osmesa, so they run in the host reference layer, not on-target.
if [ "$fail" -eq 0 ] && [ "$pass" -ge 1 ]; then echo "TEST PASSED"; else echo "TEST FAILED"; fi
