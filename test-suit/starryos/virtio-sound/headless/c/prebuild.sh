#!/bin/sh
set -eu

# The system prebuild guest mounts only this subcase directory, so the ALSA
# build recipe must be self-contained here. Keep the three VirtIO sound
# profile copies synchronized.
: "${STARRY_STAGING_ROOT:?}"

staging="$STARRY_STAGING_ROOT"
runtime_prefix="/usr"
work="${TMPDIR:-/tmp}/virtio-sound-alsa-$$"
downloads="$work/downloads"

cleanup() {
    rm -rf "$work"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$downloads"

use_musl_cross=0
cross_gcc=
cross_bin=
cross_prefix=
cross_lib=
native_path=
native_pkg_config=
target_include=
build_cppflags=
build_ldflags=
configure_alsa_args=
alsa_pkgconfig_path=
pkg_config_sysroot=
alsa_utils_ld_library_path=
if [ -n "${QEMU_LD_PREFIX:-}" ]; then
    use_musl_cross=1
    native_path=${PATH#*:}
    cross_gcc="$(
        PATH="$native_path"
        command -v x86_64-linux-musl-gcc || true
    )"
    if [ -z "$cross_gcc" ] || [ ! -x "$cross_gcc" ]; then
        echo "x86_64-linux-musl-gcc not found in native PATH; install a native x86_64-linux-musl cross toolchain" >&2
        exit 1
    fi
    cross_bin=${cross_gcc%/*}
    cross_prefix=${cross_gcc%gcc}
    cross_root=${cross_bin%/bin}
    cross_target=${cross_prefix##*/}
    cross_target=${cross_target%-}
    cross_lib="$cross_root/$cross_target/lib"
    for tool in ar as ld nm ranlib strip objcopy objdump readelf; do
        if [ ! -x "${cross_prefix}${tool}" ]; then
            echo "missing native musl cross binutils: ${cross_prefix}${tool}" >&2
            exit 1
        fi
    done
    if [ ! -f "$cross_lib/libc.so" ] || [ ! -f "$cross_lib/libgcc_s.so.1" ]; then
        echo "native musl cross sysroot is missing dynamic libc/libgcc_s in $cross_lib" >&2
        exit 1
    fi
    native_pkg_config="$(
        PATH="$native_path"
        command -v pkg-config || command -v pkgconf || true
    )"
    if [ -z "$native_pkg_config" ] || [ ! -x "$native_pkg_config" ]; then
        echo "native pkg-config/pkgconf not found in host PATH" >&2
        exit 1
    fi
    target_include="$work/target-include"
fi

# In qemu-user prebuild, LD_LIBRARY_PATH points at the target staging root for
# emulated build tools. A host curl resolves outside that emulation and must
# not load target libcurl/libunistring with the host glibc loader.
host_curl() {
    (
        unset LD_LIBRARY_PATH
        exec curl "$@"
    )
}

run_configure() {
    label="$1"
    shift
    "$@" || {
        status=$?
        echo "AXBUILD_CONFIGURE_FAILED: $label status=$status" >&2
        if [ -f config.log ]; then
            echo "----- $label config.log -----" >&2
            cat config.log >&2
            echo "----- end $label config.log -----" >&2
        else
            echo "config.log missing in $(pwd) for $label" >&2
        fi
        return "$status"
    }
}

build_alsa_tools() {
    mkdir -p "$work/build-lib"
    cd "$work/build-lib"
    run_configure alsa-lib "$work/src/alsa-lib-1.2.13/configure" \
        $configure_host_args \
        --prefix="$runtime_prefix" \
        --disable-alisp \
        --disable-aload \
        --disable-maintainer-mode \
        --disable-old-symbols \
        --disable-python \
        --disable-rawmidi \
        --disable-resmgr \
        --disable-seq \
        --disable-topology \
        --disable-ucm
    make -j2
    make install DESTDIR="$staging"

    if [ "$use_musl_cross" -eq 1 ]; then
        libasound_real="$staging/usr/lib/libasound.so.2.0.0"
        if [ ! -f "$libasound_real" ] || [ ! -f "$staging/usr/lib/pkgconfig/alsa.pc" ]; then
            echo "cross build did not install libasound or alsa.pc" >&2
            exit 1
        fi
        mkdir -p "$work/libasound" "$work/pkgconfig"
        ln -sf "$libasound_real" "$work/libasound/libasound.so.2.0.0"
        ln -sf libasound.so.2.0.0 "$work/libasound/libasound.so.2"
        ln -sf libasound.so.2.0.0 "$work/libasound/libasound.so"
        ln -sf "$staging/usr/include/alsa" "$target_include/alsa"
        sed \
            -e "s|^Libs:.*|Libs: $work/libasound/libasound.so -lm -lpthread -lrt|" \
            -e "s|^Cflags:.*|Cflags: -I$target_include|" \
            "$staging/usr/lib/pkgconfig/alsa.pc" >"$work/pkgconfig/alsa.pc"
    fi

    mkdir -p "$work/build-utils"
    cd "$work/build-utils"
    (
        export PKG_CONFIG="$pkg_config_bin"
        export PKG_CONFIG_PATH="$alsa_pkgconfig_path"
        if [ -n "$pkg_config_sysroot" ]; then
            export PKG_CONFIG_SYSROOT_DIR="$pkg_config_sysroot"
        else
            unset PKG_CONFIG_SYSROOT_DIR
        fi
        export CPPFLAGS="$build_cppflags"
        export LDFLAGS="$build_ldflags"
        if [ -n "$alsa_utils_ld_library_path" ]; then
            export LD_LIBRARY_PATH="$alsa_utils_ld_library_path"
        else
            unset LD_LIBRARY_PATH
        fi
        run_configure alsa-utils "$work/src/alsa-utils-1.2.13/configure" \
            $configure_host_args \
            $configure_alsa_args \
            --prefix="$runtime_prefix" \
            --disable-alsaconf \
            --disable-alsaloop \
            --disable-alsamixer \
            --disable-bat \
            --disable-maintainer-mode \
            --disable-nhlt \
            --disable-nls \
            --disable-rst2man \
            --disable-xmlto
    )
    make -C aplay -j2
    make -C aplay install DESTDIR="$staging"
}

# The qemu-user prebuild runs with the staging root as its emulated root and
# must use the wrapper (which disables package scripts). The full-system
# prebuild needs the tools installed in the running Alpine root, while the
# staging root only needs the Linux UAPI headers for the host cross-compile.
if [ -n "${QEMU_LD_PREFIX:-}" ]; then
    apk add build-base linux-headers bzip2 pkgconf curl ca-certificates
else
    /sbin/apk --no-cache --timeout 120 \
        --repositories-file "$staging/etc/apk/repositories" \
        --keys-dir "$staging/etc/apk/keys" \
        add build-base linux-headers bzip2 pkgconf curl ca-certificates
    apk add linux-headers
fi

if [ "$use_musl_cross" -eq 1 ]; then
    mkdir -p "$target_include"
    for dir in asm asm-generic drm linux misc mtd rdma scsi sound video xen; do
        if [ -d "$staging/usr/include/$dir" ]; then
            ln -s "$staging/usr/include/$dir" "$target_include/$dir"
        fi
    done
fi

fetch() {
    name="$1"
    url="$2"
    expected="$3"
    archive="$downloads/$name"

    if [ -f "$archive" ]; then
        actual="$(sha256sum "$archive" | awk '{print $1}')"
        if [ "$actual" = "$expected" ]; then
            return 0
        fi
    fi

    rm -f "$archive"
    host_curl -fL --retry 3 --connect-timeout 20 --max-time 300 \
        -o "$archive" "$url"
    actual="$(sha256sum "$archive" | awk '{print $1}')"
    [ "$actual" = "$expected" ] ||
        { echo "sha256 mismatch for $name: $actual" >&2; return 1; }
}

fetch alsa-lib-1.2.13.tar.bz2 \
    https://www.alsa-project.org/files/pub/lib/alsa-lib-1.2.13.tar.bz2 \
    8c4ff37553cbe89618e187e4c779f71a9bb2a8b27b91f87ed40987cc9233d8f6
fetch alsa-utils-1.2.13.tar.bz2 \
    https://www.alsa-project.org/files/pub/utils/alsa-utils-1.2.13.tar.bz2 \
    1702a6b1cdf9ba3e996ecbc1ddcf9171e6808f5961d503d0f27e80ee162f1daa

mkdir -p "$work/src"
tar -xjf "$downloads/alsa-lib-1.2.13.tar.bz2" -C "$work/src"
tar -xjf "$downloads/alsa-utils-1.2.13.tar.bz2" -C "$work/src"

if [ "$use_musl_cross" -eq 1 ]; then
    configure_host_args="--build=x86_64-pc-linux-gnu --host=x86_64-linux-musl"
    pkg_config_bin="$native_pkg_config"
    build_cppflags="-I$target_include"
    build_ldflags="-L$cross_lib -L$work/libasound"
    configure_alsa_args="--with-alsa-prefix=$work/libasound --with-alsa-inc-prefix=$target_include"
    alsa_pkgconfig_path="$work/pkgconfig"
    pkg_config_sysroot=
    alsa_utils_ld_library_path=
    (
        unset LD_LIBRARY_PATH
        export PATH="$cross_bin:$native_path"
        export CC="$cross_gcc"
        export AR="${cross_prefix}ar"
        export AS="${cross_prefix}as"
        export LD="${cross_prefix}ld"
        export NM="${cross_prefix}nm"
        export RANLIB="${cross_prefix}ranlib"
        export STRIP="${cross_prefix}strip"
        export OBJCOPY="${cross_prefix}objcopy"
        export OBJDUMP="${cross_prefix}objdump"
        export READELF="${cross_prefix}readelf"
        export CPPFLAGS="$build_cppflags"
        export LDFLAGS="$build_ldflags"
        build_alsa_tools
    )
else
    configure_host_args=
    configure_alsa_args=
    pkg_config_bin="$(command -v pkgconf || command -v pkg-config)" ||
        { echo "pkg-config provider is missing" >&2; exit 1; }
    build_cppflags="-I$staging/usr/include"
    build_ldflags="-L$staging/usr/lib"
    alsa_pkgconfig_path="$staging/usr/lib/pkgconfig:$staging/usr/share/pkgconfig"
    pkg_config_sysroot="$staging"
    alsa_utils_ld_library_path="$staging/usr/lib:$staging/lib"
    build_alsa_tools
fi

test -x "$staging/usr/bin/aplay"
test -x "$staging/usr/bin/arecord"
test -f "$staging/usr/share/alsa/alsa.conf"
