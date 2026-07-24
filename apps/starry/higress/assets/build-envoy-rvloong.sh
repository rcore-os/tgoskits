#!/usr/bin/env bash
# build-envoy-rvloong.sh - source-build Envoy 1.38.3 for riscv64 / loongarch64.
#
# Upstream Envoy ships prebuilt binaries only for glibc x86_64 + aarch64. This
# script reproduces the source build of the same Envoy release for the two
# remaining StarryOS architectures, cross-compiled with clang-18 against a musl
# cross sysroot (output: musl-dynamic ELF, interp /lib/ld-musl-<arch>.so.1,
# which the Alpine base rootfs already provides).
#
# The gateway is built with a reduced extension set - exactly the data-plane
# surface the carpet exercises (HTTP connection manager, router, local rate
# limit, TLS via BoringSSL, static clusters). The extensions that hard-require a
# per-arch runtime with no rv64/loong64 port are dropped: V8 (wasm), LuaJIT
# (lua), the Rust dynamic-modules / Hickory DNS resolver, and the Go filter.
# HTTP/3 (QUICHE) is disabled since the carpet is HTTP/1 + TLS only.
#
# Usage:  build-envoy-rvloong.sh <riscv64|loongarch64> [out-dir]
# Env:    ENVOY_SRC (reuse an existing checkout), BAZEL (bazelisk path),
#         *_MUSL_CROSS (override the cross toolchain root), JOBS.
set -euo pipefail

arch="${1:?usage: build-envoy-rvloong.sh <riscv64|loongarch64> [out-dir]}"
out_dir="${2:-$PWD}"
ENVOY_VER=1.38.3
PLATFORMS_VER=1.1.0
PLATFORMS_SHA=324f5381753a610e472f79563d44e2026438195042aae4dc660b8c021f7de7f5
work="${ENVOY_BUILD_ROOT:-${HOME}/.cache/starry-higress-envoy}"
mkdir -p "$work"

case "$arch" in
    riscv64)     triple=riscv64-linux-musl ;;
    loongarch64) triple=loongarch64-linux-musl ;;
    *) echo "unknown arch: $arch (want riscv64|loongarch64)" >&2; exit 1 ;;
esac

# --- 1. cross toolchain (clang-18 + <arch>-linux-musl-cross for sysroot/binutils/libstdc++) ---
clang_bin="${CLANG18:-$(command -v clang-18 || command -v clang)}"
[ -x "$clang_bin" ] || { echo "clang-18 not found (Envoy needs clang>=18)" >&2; exit 1; }
cross="${MUSL_CROSS:-}"
if [ -z "$cross" ]; then
    for c in "/opt/musl/$triple-cross" "$work/$triple-cross"; do
        [ -x "$c/bin/$triple-gcc" ] && cross="$c" && break
    done
fi
if [ -z "$cross" ] || [ ! -x "$cross/bin/$triple-gcc" ]; then
    tgz="${MUSL_CROSS_TGZ:-${HOME}/rcore/download/${triple}-cross.tgz}"
    [ -f "$tgz" ] || { echo "cross toolchain not found; set MUSL_CROSS or MUSL_CROSS_TGZ" >&2; exit 1; }
    tar -xzf "$tgz" -C "$work"; cross="$work/$triple-cross"
fi
gcc_ver="$("$cross/bin/$triple-gcc" -dumpversion)"
echo "toolchain: clang $("$clang_bin" -dumpversion) targeting $triple, libstdc++ from gcc $gcc_ver ($cross)"

# --- 1b. libstdc++ <valarray> / clang fix ---
# gcc 11's bits/range_access.h forward-declares the valarray begin/end overloads
# without noexcept, while <valarray> defines them with noexcept. gcc tolerates the
# mismatch; clang rejects it ("exception specification ... does not match"), which
# breaks every TU that includes <valarray> (yaml-cpp, ...). Add noexcept to the
# forward decls - the same fix later gcc releases shipped.
# The patched copy goes into a private overlay ($work/cxx-overlay/bits/) so the
# caller's toolchain is never modified; the cc/cxx wrappers add -isystem to prefer
# the overlay over the gcc toolchain's original header.
cxx_overlay="$work/cxx-overlay"
mkdir -p "$cxx_overlay/bits"
for rah in "$cross"/*/include/c++/*/bits/range_access.h; do
    [ -f "$rah" ] || continue
    dest="$cxx_overlay/bits/range_access.h"
    if grep -q 'begin(valarray<_Tp>&) noexcept;' "$rah"; then
        cp "$rah" "$dest"
    else
        sed -E 's/(begin|end)\((const )?valarray<_Tp>&\);/\1(\2valarray<_Tp>\&) noexcept;/' \
            "$rah" > "$dest"
        echo "overlay: patched bits/range_access.h noexcept in $cxx_overlay (toolchain untouched)"
    fi
done

# --- 2. Envoy source (pinned tag) ---
src="${ENVOY_SRC:-$work/envoy-$ENVOY_VER}"
if [ ! -d "$src/.git" ]; then
    git clone --depth 1 --branch "v$ENVOY_VER" https://github.com/envoyproxy/envoy.git "$src"
fi
cd "$src"

# --- 3. patch: register riscv64 / loongarch64 as Linux CPU config_settings ---
python3 - "$arch" <<'PY'
import re, sys
p = "bazel/BUILD"; s = open(p).read()
if "name = \"linux_riscv64\"" not in s:
    anchor = 'config_setting(\n    name = "linux",'
    inject = '''config_setting(
    name = "linux_riscv64",
    constraint_values = [
        "@platforms//cpu:riscv64",
        "@platforms//os:linux",
    ],
)

config_setting(
    name = "linux_loongarch64",
    constraint_values = [
        "@platforms//cpu:loongarch64",
        "@platforms//os:linux",
    ],
)

'''
    s = s.replace(anchor, inject + anchor, 1)
    s = s.replace('        ":linux_s390x",\n    ],\n)',
                  '        ":linux_s390x",\n        ":linux_riscv64",\n        ":linux_loongarch64",\n    ],\n)', 1)
    open(p, "w").write(s)
    print("patched bazel/BUILD: linux_riscv64 + linux_loongarch64 config_settings")
PY

# --- 4. patch: bump @platforms 1.0.0 -> 1.1.0 (1.0.0 lacks the loongarch64 cpu constraint) ---
python3 - "$PLATFORMS_VER" "$PLATFORMS_SHA" <<'PY'
import sys
ver, sha = sys.argv[1], sys.argv[2]
p = "bazel/repository_locations.bzl"; s = open(p).read()
s = s.replace('''    platforms = dict(
        version = "1.0.0",
        sha256 = "852b71bfa15712cec124e4a57179b6bc95d59fdf5052945f5d550e072501a769",''',
    f'''    platforms = dict(
        version = "{ver}",
        sha256 = "{sha}",''', 1)
open(p, "w").write(s)
print("patched repository_locations.bzl: platforms ->", ver)
PY

# --- 4b. BoringSSL: recognize loongarch64 ---
# The pinned BoringSSL target.h enumerates x86/arm/riscv/mips/... but not
# loongarch64, so every TU that includes it fails with "Unknown target CPU" on the
# loong build (riscv64 already has a case). Add a no-asm loongarch64 case (parallel
# to riscv64) through the repository patch list BoringSSL is already fetched with.
cat > bazel/boringssl-loongarch64-target.patch <<'PATCH'
--- a/include/openssl/target.h
+++ b/include/openssl/target.h
@@ -45,6 +45,11 @@
 #define OPENSSL_RISCV64
 #elif defined(__riscv) && __SIZEOF_POINTER__ == 4
 #define OPENSSL_32_BIT
+#elif defined(__loongarch__) && __SIZEOF_POINTER__ == 8
+#define OPENSSL_64_BIT
+#define OPENSSL_LOONGARCH64
+#elif defined(__loongarch__) && __SIZEOF_POINTER__ == 4
+#define OPENSSL_32_BIT
 #elif defined(__pnacl__)
 #define OPENSSL_32_BIT
 #define OPENSSL_PNACL
PATCH
grep -q "boringssl-loongarch64-target.patch" bazel/repositories.bzl || \
    sed -i 's#\("@envoy//bazel:boringssl-bssl-compat.patch",\)#\1\n            "@envoy//bazel:boringssl-loongarch64-target.patch",#' bazel/repositories.bzl

# --- 4c. brotli: no "small" code model on loongarch64 ---
# brotli's platform.h enables __attribute__((model("small"))) whenever the compiler
# has the model attribute (clang does), but loongarch64 has no "small" code model
# (only normal/medium/large) - "code model 'small' is not supported". Exclude
# loongarch so BROTLI_MODEL is a no-op there (x86/rv64 keep it). _brotli() has no
# patches list, so add one.
cat > bazel/brotli-loongarch64-model.patch <<'PATCH'
--- a/c/common/platform.h
+++ b/c/common/platform.h
@@ -668,4 +668,4 @@
-#if BROTLI_GNUC_HAS_ATTRIBUTE(model, 3, 0, 3)
+#if BROTLI_GNUC_HAS_ATTRIBUTE(model, 3, 0, 3) && !defined(__loongarch__)
 #define BROTLI_MODEL(M) __attribute__((model(M)))
 #else
 #define BROTLI_MODEL(M) /* M */
PATCH
python3 - <<'PY'
p = "bazel/repositories.bzl"; s = open(p).read()
if "brotli-loongarch64-model.patch" not in s:
    s = s.replace(
        'def _brotli():\n    external_http_archive(\n        name = "brotli",\n    )',
        'def _brotli():\n    external_http_archive(\n        name = "brotli",\n        patches = ["@envoy//bazel:brotli-loongarch64-model.patch"],\n        patch_args = ["-p1"],\n    )', 1)
    open(p, "w").write(s)
    print("registered brotli-loongarch64-model.patch in _brotli()")
PY

# --- 5. trim extensions: drop the families that need a per-arch runtime with no
# rv64/loong64 port, or a toolchain the cross build cannot satisfy:
#   wasm (V8/WAMR/wasmtime), lua (LuaJIT), golang (cgo filter), dynamic_modules
#   (Rust ABI), hickory (Rust DNS resolver), and the datadog / opentelemetry
#   tracer families (dd-trace-cpp pulls <valarray>, which GNU libstdc++ 11 exposes
#   in a form clang rejects; opentelemetry pulls cel-cpp / otel-cpp). None are on
#   the gateway data path the carpet exercises.
python3 - <<'PY'
import re
p = "source/extensions/extensions_build_config.bzl"
out, n = [], 0
drop = re.compile(r'dynamic_module|wasm|lua|golang|hickory|datadog|opentelemetry')
for line in open(p):
    st = line.lstrip()
    if st.startswith('"envoy.') and drop.search(line):
        out.append("    # [starry-rvloong dropped] " + st); n += 1
    else:
        out.append(line)
open(p, "w").write("".join(out))
print("trimmed", n, "extension entries (wasm/lua/golang/dynamic_modules/hickory/datadog/opentelemetry)")
PY

# --- 5b. drop Envoy's -Werror ---
# envoy_cc_library appends envoy_copts() (-Wextra -Werror ...) after our --copt, so a
# global -Wno-error/-Wno-sign-compare copt cannot override it. Envoy's strict warning
# set is validated only on its supported glibc targets; on musl/rv64/loong64 a few
# benign pedantic warnings (e.g. -Wsign-compare from differing libc typedefs) would
# abort the build. Drop -Werror from envoy_copts - warnings stay visible. Idempotent.
sed -i '/^[[:space:]]*"-Werror",[[:space:]]*$/d' bazel/envoy_internal.bzl
grep -q '"-Werror"' bazel/envoy_internal.bzl \
    && echo "warn: -Werror still present in bazel/envoy_internal.bzl" >&2 \
    || echo "dropped -Werror from envoy_copts"

# --- 6. Bazel cross cc_toolchain (clang-18 + musl cross) ---
tc="bazel/starry_cross/$arch"; mkdir -p "$tc/bin"
sysroot="$cross/$triple"
cat > "$tc/bin/cc" <<EOF
#!/bin/sh
exec "$clang_bin" --target=$triple --sysroot=$sysroot --gcc-toolchain=$cross \\
  -B$cross/lib/gcc/$triple/$gcc_ver -B$sysroot/bin -fuse-ld=$cross/bin/$triple-ld \\
  ${cxx_overlay:+-isystem "$cxx_overlay"} "\$@"
EOF
cat > "$tc/bin/cxx" <<EOF
#!/bin/sh
exec "${clang_bin}++" --target=$triple --sysroot=$sysroot --gcc-toolchain=$cross \\
  -B$cross/lib/gcc/$triple/$gcc_ver -B$sysroot/bin -fuse-ld=$cross/bin/$triple-ld -stdlib=libstdc++ \\
  ${cxx_overlay:+-isystem "$cxx_overlay"} "\$@"
EOF
chmod +x "$tc/bin/cc" "$tc/bin/cxx"
# Real exec wrappers (not symlinks): bazel's linux-sandbox will not resolve a
# staged symlink pointing outside the workspace, so directly-invoked tools like
# ar/strip must be scripts that exec the absolute cross-tool path.
for t in ar as ld nm objcopy objdump strip ranlib readelf; do
    [ -x "$cross/bin/$triple-$t" ] && { rm -f "$tc/bin/$t"; printf '#!/bin/sh\nexec "%s" "$@"\n' "$cross/bin/$triple-$t" > "$tc/bin/$t"; chmod +x "$tc/bin/$t"; }
done
for t in dwp gcov; do [ -e "$tc/bin/$t" ] || { printf '#!/bin/sh\nexit 0\n' > "$tc/bin/$t"; chmod +x "$tc/bin/$t"; }; done

cpu="$arch"; [ "$arch" = riscv64 ] && bzlcpu=riscv64 || bzlcpu=loongarch64
# clang builtin/resource include dir. With -no-canonical-prefixes clang derives it
# relative to the invoked binary (e.g. /usr/bin/clang-18 -> /usr/lib/clang/N),
# which can differ from -print-resource-dir; declare both so bazel treats the
# compiler's own headers (arm_neon.h, stddef.h, ...) as builtin instead of raising
# absolute-path include violations.
clang_major="$("$clang_bin" -dumpversion | cut -d. -f1)"
nc_resinc="$(dirname "$(dirname "$clang_bin")")/lib/clang/$clang_major/include"
cat > "bazel/starry_cross/BUILD" <<EOF
load(":cc_toolchain_config.bzl", "cc_toolchain_config")
package(default_visibility = ["//visibility:public"])
platform(name = "$arch", constraint_values = ["@platforms//cpu:$bzlcpu", "@platforms//os:linux"])
cc_toolchain_config(name = "${arch}_config", target_cpu = "$bzlcpu",
    tool_dir = "$arch/bin",
    builtin_includes = ["$cross", "$($clang_bin -print-resource-dir)/include", "$nc_resinc"])
filegroup(name = "${arch}_files", srcs = glob(["$arch/bin/**"]))
cc_toolchain(name = "${arch}_cc", toolchain_config = ":${arch}_config",
    all_files = ":${arch}_files", ar_files = ":${arch}_files", as_files = ":${arch}_files",
    compiler_files = ":${arch}_files", dwp_files = ":${arch}_files",
    linker_files = ":${arch}_files", objcopy_files = ":${arch}_files", strip_files = ":${arch}_files",
    supports_param_files = 0)
toolchain(name = "${arch}_toolchain",
    exec_compatible_with = ["@platforms//cpu:x86_64", "@platforms//os:linux"],
    target_compatible_with = ["@platforms//cpu:$bzlcpu", "@platforms//os:linux"],
    toolchain = ":${arch}_cc", toolchain_type = "@bazel_tools//tools/cpp:toolchain_type")
EOF
cp "$(dirname "$0")/starry-cross-cc-toolchain-config.bzl" "bazel/starry_cross/cc_toolchain_config.bzl" 2>/dev/null || \
cat > "bazel/starry_cross/cc_toolchain_config.bzl" <<'EOF'
load("@bazel_tools//tools/cpp:cc_toolchain_config_lib.bzl", "feature", "flag_group", "flag_set", "tool_path")
load("@bazel_tools//tools/build_defs/cc:action_names.bzl", "ACTION_NAMES")
_C = [ACTION_NAMES.c_compile, ACTION_NAMES.cpp_compile, ACTION_NAMES.cpp_header_parsing,
      ACTION_NAMES.cpp_module_compile, ACTION_NAMES.assemble, ACTION_NAMES.preprocess_assemble]
_L = [ACTION_NAMES.cpp_link_executable, ACTION_NAMES.cpp_link_dynamic_library,
      ACTION_NAMES.cpp_link_nodeps_dynamic_library]
def _impl(ctx):
    t = ctx.attr.tool_dir
    tp = [tool_path(name = n, path = t + "/" + b) for n, b in [
        ("gcc", "cc"), ("cpp", "cc"), ("ar", "ar"), ("ld", "ld"), ("nm", "nm"),
        ("objcopy", "objcopy"), ("objdump", "objdump"), ("strip", "strip"),
        ("gcov", "gcov"), ("dwp", "dwp")]]
    df = feature(name = "default_flags", enabled = True, flag_sets = [
        flag_set(actions = _C, flag_groups = [flag_group(flags = [
            "-no-canonical-prefixes", "-fPIC", "-Wno-unused-command-line-argument"])]),
        flag_set(actions = _L, flag_groups = [flag_group(flags = ["-no-canonical-prefixes", "-lm"])])])
    return cc_common.create_cc_toolchain_config_info(
        ctx = ctx, toolchain_identifier = "starry-" + ctx.attr.target_cpu,
        host_system_name = "local", target_system_name = ctx.attr.target_cpu + "-linux-musl",
        target_cpu = ctx.attr.target_cpu, target_libc = "musl", compiler = "clang",
        abi_version = "clang-18", abi_libc_version = "musl", tool_paths = tp,
        cxx_builtin_include_directories = ctx.attr.builtin_includes,
        features = [df, feature(name = "opt"), feature(name = "dbg"),
                    feature(name = "supports_pic", enabled = True)])
cc_toolchain_config = rule(implementation = _impl, provides = [CcToolchainConfigInfo], attrs = {
    "target_cpu": attr.string(mandatory = True), "tool_dir": attr.string(mandatory = True),
    "builtin_includes": attr.string_list(mandatory = True)})
EOF

cat > "starry-cross.bazelrc" <<EOF
build --incompatible_enable_cc_toolchain_resolution
build --extra_toolchains=//bazel/starry_cross:${arch}_toolchain
build --define=wasm=disabled
build --//bazel:http3=false
build --define=tcmalloc=disabled
build --define=signal_trace=disabled
build --define=hot_restart=disabled
build --define=google_grpc=disabled
build --define=deprecated_features=disabled
build --copt=-Wno-unused-command-line-argument
build --host_copt=-Wno-unused-command-line-argument
# flatbuffers keys its locale-aware strtoll_l/strtoull_l/strtod_l on _XOPEN_VERSION,
# which musl reports without providing the *_l functions; force the portable path.
build --copt=-DFLATBUFFERS_LOCALE_INDEPENDENT=0
build --host_copt=-DFLATBUFFERS_LOCALE_INDEPENDENT=0
# Envoy compiles its own sources with -Werror and a strict -Wextra set validated
# only on its supported (glibc) targets; on the musl/rv64/loong64 cross target a
# few benign pedantic warnings (e.g. -Wsign-compare from differing libc typedefs)
# would otherwise abort the build. Downgrade -Werror to warnings - correctness is
# gated by the runtime carpet, not by these host-platform lint flags.
build --copt=-Wno-error
build --host_copt=-Wno-error
# quiche (compiled even with http3 off) uses int32_t via transitive includes that
# the loong cross's newer gcc-13 libstdc++ no longer pulls in ("unknown type name
# 'int32_t'"); force-include <cstdint> for quiche's C++ TUs. Harmless on the rv
# cross (gcc-11 already provides it).
build --per_file_copt=external/quiche/.*@-include,cstdint
build --verbose_failures
EOF

# --- 7. prefetch the hermetic host LLVM (exec-config compiler) into a distdir ---
# Envoy's WORKSPACE pulls clang+llvm-18.1.8 (~1GB) for the exec toolchain that
# builds host tools (protoc, ...). Prefetching avoids flaky mid-download aborts.
distdir="$work/distdir"; mkdir -p "$distdir"
llvm_tar="clang+llvm-18.1.8-x86_64-linux-gnu-ubuntu-18.04.tar.xz"
llvm_sha=54ec30358afcc9fb8aa74307db3046f5187f9fb89fb37064cdde906e062ebf36
if ! echo "$llvm_sha  $distdir/$llvm_tar" | sha256sum -c - >/dev/null 2>&1; then
    curl -fL -C - --retry 10 --retry-all-errors -o "$distdir/$llvm_tar" \
        "https://github.com/llvm/llvm-project/releases/download/llvmorg-18.1.8/$llvm_tar"
    echo "$llvm_sha  $distdir/$llvm_tar" | sha256sum -c -
fi

# --- 7b. libtinfo.so.5 for the prebuilt LLVM host toolchain ---
# The clang+llvm-18.1.8 exec toolchain links libtinfo.so.5 (ncurses5), which
# modern distros (Ubuntu 24.04) no longer ship. Unprivileged fix: cache the lib
# under $work/lib/ and point Bazel's exec actions at it via --action_env. If
# patchelf is available, also embed the rpath into the clang binary directly so
# sandbox environments that do not inherit LD_LIBRARY_PATH still work.
libtinfo_lib="$work/lib"
mkdir -p "$libtinfo_lib"
if ! ldconfig -p 2>/dev/null | grep -q 'libtinfo\.so\.5\b' && \
   [ ! -f "$libtinfo_lib/libtinfo.so.5" ]; then
    deb="$work/libtinfo5.deb"
    for u in \
        https://archive.ubuntu.com/ubuntu/pool/universe/n/ncurses/libtinfo5_6.2-0ubuntu2.1_amd64.deb \
        https://security.ubuntu.com/ubuntu/pool/universe/n/ncurses/libtinfo5_6.2-0ubuntu2.1_amd64.deb \
        https://archive.ubuntu.com/ubuntu/pool/universe/n/ncurses/libtinfo5_6.3-2ubuntu0.1_amd64.deb ; do
        curl -fsSL --retry 3 "$u" -o "$deb" && break
    done
    [ -f "$deb" ] || { echo "build: could not fetch libtinfo5.deb" >&2; exit 1; }
    tmpx="$work/libtinfo5-x"; rm -rf "$tmpx"; mkdir -p "$tmpx"
    dpkg-deb -x "$deb" "$tmpx" 2>/dev/null || ( cd "$tmpx" && ar x "$deb" && tar -xf data.tar.* )
    so="$(find "$tmpx" -name 'libtinfo.so.5.*' -type f | head -1)"
    [ -n "$so" ] || { echo "build: libtinfo.so.5.* not found in deb" >&2; exit 1; }
    install -m0644 "$so" "$libtinfo_lib/libtinfo.so.5"
    rm -rf "$tmpx" "$deb"
    echo "cached libtinfo.so.5 -> $libtinfo_lib (no system install)"
fi
# Propagate into every Bazel action: mount our lib dir inside the sandbox so
# LD_LIBRARY_PATH can reach it even with linux-sandbox's mount namespace.
if ! ldconfig -p 2>/dev/null | grep -q 'libtinfo\.so\.5\b' && \
   [ -f "$libtinfo_lib/libtinfo.so.5" ]; then
    # --sandbox_add_mount_pair mounts the host path at the same path inside the
    # sandbox; combined with --action_env the exec clang-18 can dlopen libtinfo.
    echo "build --sandbox_add_mount_pair=$libtinfo_lib:$libtinfo_lib" >> starry-cross.bazelrc
    echo "build --action_env=LD_LIBRARY_PATH=$libtinfo_lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
        >> starry-cross.bazelrc
    # Belt-and-suspenders: if patchelf is available and the Bazel output_base
    # already holds an extracted LLVM repo (from a prior run), bake $ORIGIN/../lib
    # into the clang-18 rpath so the binary carries the hint independently.
    if command -v patchelf >/dev/null 2>&1; then
        _ob="$("${BAZEL:-bazel}" info output_base 2>/dev/null || true)"
        _llvm="$_ob/external/clang_llvm_18_1_8_x86_64_linux_gnu_ubuntu_18_04"
        if [ -d "$_llvm/bin" ]; then
            mkdir -p "$_llvm/lib"
            cp -n "$libtinfo_lib/libtinfo.so.5" "$_llvm/lib/" 2>/dev/null || true
            for _cb in "$_llvm/bin/clang-18" "$_llvm/bin/clang"; do
                [ -f "$_cb" ] && patchelf --add-rpath '$ORIGIN/../lib' "$_cb" 2>/dev/null && \
                    echo "patchelf: rpath added to $(basename "$_cb") in output_base"
            done
        fi
    fi
fi

# --- 8. build ---
BAZEL="${BAZEL:-bazel}"
export USE_BAZEL_VERSION="$(cat .bazelversion)"
# Link with LLVM lld, not the musl GNU ld: the final envoy-static link wraps ~1400
# whole-archive groups whose objects carry tens of thousands of -ffunction-sections
# sections; old GNU ld/BFD spuriously rejects a member ("... is not an object") on
# that scale, while lld (Envoy's own linker) handles it. --linkopt only re-keys the
# link action, so it does not invalidate the compiled objects. lld ships with clang.
# The exe link runs through the C-driver cc wrapper (no -stdlib=libstdc++), so add
# -lstdc++ explicitly; dynamic libstdc++.so.6 is used (GCC's static libstdc++.a has
# .eh_frame relocations into discarded COMDAT sections that lld rejects) and the app
# prebuild stages libstdc++.so.6 alongside the other NEEDED sonames.
"$BAZEL" --bazelrc=starry-cross.bazelrc build -c opt --define=no_debug_info=1 --fission=no \
    --linkopt=-fuse-ld=lld --linkopt=-lstdc++ \
    --distdir="$distdir" \
    --jobs="${JOBS:-6}" --local_resources=memory="${RAM_MB:-6144}" \
    --platforms="//bazel/starry_cross:$arch" \
    //source/exe:envoy-static

install -Dm0755 "bazel-bin/source/exe/envoy-static" "$out_dir/envoy-$ENVOY_VER-linux-$arch"
echo "built: $out_dir/envoy-$ENVOY_VER-linux-$arch"
file "$out_dir/envoy-$ENVOY_VER-linux-$arch" 2>/dev/null || true
