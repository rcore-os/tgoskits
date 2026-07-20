#!/usr/bin/env bash
# prebuild.sh -- provision the StarryOS `monitor` app overlay, reproducibly:
#   * the Prometheus monitoring stack -- prometheus 3.11.3 + promtool (fully-static CGO-free Go
#     binaries) + node_exporter 1.11.1 (the simplest exporter, a static Go binary), fetched at
#     build time from the official GitHub releases (per-arch tarball, sha256-verified). loong64
#     upstream ships NO prebuilt, so it is Go-cross-compiled from the pinned source tag (or taken
#     from a maintainer-staged reproducible artifact) -- NEVER a SKIP. See assets/MANIFEST.md.
#   * the glances system monitor -- `apk add glances` (Alpine musl, all 4 arches) plus its FastAPI/
#     Starlette/pydantic/uvicorn web closure, resolved INTO the staging tree by qemu-user-static so
#     apk pulls the CURRENT version + the full musl .so closure for the TARGET arch (no drifting
#     hardcoded apk URLs, no cache-miss exit). pyte + uvicorn are not in Alpine v3.23, so their
#     pinned pure-python (noarch) wheels are fetched by URL + sha256 and unpacked into site-packages.
#   * the on-target carpet suite (prometheus + glances CLI/headless/TUI-pyte/client-server/web) and
#     the pyte harness (pty_tui_drive.py + pyte_assert.py) staged under /root/monitor.
#
# NOTHING binary is committed to the source tree; every binary/wheel/apk is fetched at build time.
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS, STARRY_STAGING_ROOT, STARRY_OVERLAY_DIR,
# STARRY_APP_DIR. App-specific overrides documented inline below.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"

APK_BRANCH="${MONITOR_APK_BRANCH:-v3.23}"
ALPINE_CDN="${ALPINE_CDN:-https://dl-cdn.alpinelinux.org/alpine}"
ROOTFS_SIZE="${MONITOR_ROOTFS_SIZE:-6G}"   # grafana public/ + Go binaries + glances closure are large
APK_CACHE="${MONITOR_APK_CACHE:-}"
# Optional: a maintainer-staged directory of reproducible binaries (used offline and, crucially,
# for the loong64 prometheus/node_exporter/grafana that upstream does not prebuild). Layout:
#   $MONITOR_BINS_DIR/prometheus/<arch>/prometheus-3.11.3.linux-<goarch>.tar.gz
#   $MONITOR_BINS_DIR/node_exporter/<arch>/node_exporter
#   $MONITOR_BINS_DIR/grafana/<arch>/grafana-13.0.1.linux-<goarch>.tar.gz
#   $MONITOR_BINS_DIR/grafana/grafana-13.0.1.db   (optional pre-migrated, arch-independent seed)
BINS_DIR="${MONITOR_BINS_DIR:-}"
GRAFANA_PREMIGRATE="${MONITOR_GRAFANA_PREMIGRATE:-1}"  # opportunistic build-time SQLite migration (skips 709 on-target)

PROM_VER="3.11.3"
NE_VER="1.11.1"
GRAFANA_VER="13.0.1"
PYTE_WHL_URL="https://files.pythonhosted.org/packages/59/d0/bb522283b90853afbf506cd5b71c650cf708829914efd0003d615cf426cd/pyte-0.8.2-py3-none-any.whl"
PYTE_WHL_SHA="85db42a35798a5aafa96ac4d8da78b090b2c933248819157fc0e6f78876a0135"
UVICORN_WHL_URL="https://files.pythonhosted.org/packages/61/14/33a3a1352cfa71812a3a21e8c9bfb83f60b0011f5e36f2b1399d51928209/uvicorn-0.34.0-py3-none-any.whl"
UVICORN_WHL_SHA="023dc038422502fa28a09c7a30bf2b6991512da7dcdb8fd35fe57cfc154126f4"

case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static";     goarch="arm64"   ;;
    riscv64)     qemu_runner="qemu-riscv64-static";     goarch="riscv64" ;;
    x86_64)      qemu_runner="qemu-x86_64-static";      goarch="amd64"   ;;
    loongarch64) qemu_runner="qemu-loongarch64-static"; goarch="loong64" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

# Official release sha256 per goarch (amd64/arm64/riscv64 == upstream sha256sums.txt, verified
# byte-for-byte). loong64 has no official release -> resolved via cross-compile / staged artifact.
prom_sha() { case "$1" in
    amd64)   echo 9479af67673316278958cda1f39b88a09f8921084e039c65acca060d0447bb38 ;;
    arm64)   echo d2ec0a96259afde955ad1560ced303cef99cac4dac676bd4dd7614d76adb708a ;;
    riscv64) echo bd6978937d64f4afa82919e0c4b3b83ace50808b953ab6174e480ca7dda2ba9a ;;
    *) echo "" ;;
esac ; }
ne_sha() { case "$1" in     # node_exporter release TARBALL sha256 (upstream sha256sums.txt)
    amd64)   echo 9f5ea48e5bc7b656f8a91a32e7d7deb89f70f73dabd0d974418aca15f37d6810 ;;
    arm64)   echo ba1886efbd76cb96b0087c695ea8d1b9cb6e8aa946c996d744e9ee16c8e3591a ;;
    riscv64) echo 8d73447c47488a94f7eba467838c815ea7dceb449c75b1b8e91fa6dc3e0e364e ;;
    *) echo "" ;;
esac ; }
grafana_sha() { case "$1" in  # grafana OSS TARBALL sha256 (dl.grafana.com *.sha256; amd64/arm64/riscv64 official)
    amd64)   echo 187ddc4badb69aecb7cd3fae2884add7ed21adde7124a6f8093b7b4033d722f2 ;;
    arm64)   echo 553d5ee3fb1600c83ef2fbf336579ed6cc64fffc328843ea7d662f85b876c261 ;;
    riscv64) echo 233ac9bf87390f203e45a1beb47630b28d3eb0c0dce3bfc5838e0e1603eb2cee ;;
    *) echo "" ;;   # loong64: self-cross-compiled backend + official frontend (non-deterministic); staged
esac ; }

ensure_host_tools() {
    local missing=()
    for t in debugfs resize2fs e2fsck truncate readelf curl tar sha256sum unzip; do
        command -v "$t" >/dev/null 2>&1 || missing+=("$t")
    done
    command -v "$qemu_runner" >/dev/null 2>&1 || missing+=("$qemu_runner")
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "prebuild: missing host tools: ${missing[*]}" >&2
        command -v apt-get >/dev/null 2>&1 && {
            apt-get update && apt-get install -y --no-install-recommends \
                e2fsprogs coreutils binutils curl tar coreutils unzip qemu-user-static || true
        }
    fi
}

grow_rootfs() {
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    local before; before=$(stat -c %s "$base_rootfs")
    local target; target=$(numfmt --from=iec "${ROOTFS_SIZE/G/G}" 2>/dev/null || echo $((4*1024*1024*1024)))
    echo "prebuild: rootfs $base_rootfs is $((before/1024/1024)) MiB; growing to $ROOTFS_SIZE (grow-only)"
    if [[ "$before" -lt "$target" ]]; then
        truncate -s "$ROOTFS_SIZE" "$base_rootfs"
        e2fsck -f -y "$base_rootfs" >/dev/null 2>&1 || true
        resize2fs "$base_rootfs" >/dev/null 2>&1 || { echo "prebuild: resize2fs failed" >&2; exit 2; }
    fi
    echo "prebuild: rootfs sized to $(( $(stat -c %s "$base_rootfs")/1024/1024 )) MiB"
}

extract_base_rootfs() {
    rm -rf "$staging_root"; mkdir -p "$staging_root"
    debugfs -R "rdump / $staging_root" "$base_rootfs" >/dev/null 2>&1
    [[ -x "$staging_root/sbin/apk" ]] || { echo "prebuild: base rootfs has no apk" >&2; exit 2; }
}

normalize_symlinks() {
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"; [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

install_glances_closure() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    printf '%s/%s/main\n%s/%s/community\n' \
        "$ALPINE_CDN" "$APK_BRANCH" "$ALPINE_CDN" "$APK_BRANCH" \
        > "$staging_root/etc/apk/repositories"
    local cache_args=(); [[ -n "$APK_CACHE" ]] && { mkdir -p "$APK_CACHE"; cache_args=(--cache-dir "$APK_CACHE"); }
    # glances + psutil (core) + the FastAPI/Starlette/pydantic/uvicorn web-server closure that
    # `glances -w` needs (py3-uvicorn is not in Alpine -> vendored as a wheel below) + py3-wcwidth
    # (pyte's only dep) + jinja2 (web templates) + py3-shtab (enables glances --print-completion) +
    # htop (the ncurses process-monitor TUI, pulls its ncurses .so + terminfo closure). apk resolves
    # the full musl .so closure per arch.
    local pkgs="glances htop python3 py3-psutil py3-fastapi py3-starlette py3-pydantic py3-anyio py3-sniffio py3-h11 py3-click py3-wcwidth py3-jinja2 py3-shtab"
    echo "prebuild: apk add ($APK_BRANCH) via $qemu_runner: $pkgs"
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" --keys-dir "$staging_root/etc/apk/keys" \
            "${cache_args[@]}" --update-cache --no-progress --no-scripts add $pkgs
    [[ -e "$staging_root/usr/bin/glances" ]] || { echo "prebuild: glances not provisioned" >&2; exit 3; }
    [[ -e "$staging_root/usr/bin/htop" ]] || { echo "prebuild: htop not provisioned" >&2; exit 3; }
    local pyver; pyver="$(ls -d "$staging_root"/usr/lib/python3.* 2>/dev/null | grep -oE 'python3\.[0-9]+' | head -1)"
    case "$pyver" in python3.1[2-9]|python3.2[0-9]) echo "prebuild: provisioned glances + $pyver" ;;
        *) echo "prebuild: need CPython>=3.12, got '$pyver'" >&2; exit 3 ;; esac
}

vendor_wheel() {  # $1=url $2=sha $3=name -- fetch pinned noarch wheel, verify, unzip into site-packages
    local url="$1" sha="$2" name="$3"
    local pyver; pyver="$(ls -d "$staging_root"/usr/lib/python3.* 2>/dev/null | grep -oE 'python3\.[0-9]+' | head -1)"
    local sp="$staging_root/usr/lib/$pyver/site-packages"
    mkdir -p "$sp"; local whl="$staging_root/tmp/$name.whl"; mkdir -p "$staging_root/tmp"
    echo "prebuild: fetch wheel $name"
    curl -fL --retry 3 -o "$whl" "$url"
    echo "$sha  $whl" | sha256sum -c - || { echo "prebuild: $name wheel sha256 MISMATCH" >&2; exit 4; }
    unzip -oq "$whl" -d "$sp"; rm -f "$whl"
    echo "prebuild: vendored $name into $sp"
}

install_prometheus_stack() {
    local prom_bin_dir="$staging_root/usr/local/bin" ne_bin_dir="$staging_root/usr/bin"
    mkdir -p "$prom_bin_dir" "$ne_bin_dir"
    local topdir="prometheus-$PROM_VER.linux-$goarch"
    local tb="$staging_root/tmp/prom.tgz" ne_tb="$staging_root/tmp/ne.tgz"
    mkdir -p "$staging_root/tmp"

    # --- prometheus + promtool ---
    if [[ "$goarch" == "loong64" ]]; then
        # upstream ships no loong64: use the maintainer-staged reproducible artifact (built from the
        # pinned source tag per assets/build-loong-binaries.sh) -- NEVER a SKIP.
        local staged="${BINS_DIR:+$BINS_DIR/prometheus/$arch/prometheus-$PROM_VER.linux-loong64.tar.gz}"
        if [[ -n "$staged" && -f "$staged" ]]; then
            echo "prebuild: loong64 prometheus from staged artifact $staged"; cp -f "$staged" "$tb"
        else
            echo "prebuild: loong64 prometheus not staged -> cross-compiling from source (assets/build-loong-binaries.sh)"
            MONITOR_LOONG_OUT="$staging_root/tmp" bash "$app_dir/assets/build-loong-binaries.sh" prometheus \
                || { echo "prebuild: loong64 prometheus cross-compile FAILED (set MONITOR_BINS_DIR to a staged artifact)" >&2; exit 5; }
            cp -f "$staging_root/tmp/prometheus-$PROM_VER.linux-loong64.tar.gz" "$tb"
        fi
    else
        local url="https://github.com/prometheus/prometheus/releases/download/v$PROM_VER/$topdir.tar.gz"
        local staged="${BINS_DIR:+$BINS_DIR/prometheus/$arch/$topdir.tar.gz}"
        if [[ -n "$staged" && -f "$staged" ]]; then cp -f "$staged" "$tb"; echo "prebuild: prometheus from staged $staged"
        else echo "prebuild: download prometheus $url"; curl -fL --retry 3 -o "$tb" "$url"; fi
        echo "$(prom_sha "$goarch")  $tb" | sha256sum -c - || { echo "prebuild: prometheus sha256 MISMATCH ($goarch)" >&2; exit 5; }
    fi
    tar xzf "$tb" -C "$staging_root/tmp" "$topdir/prometheus" "$topdir/promtool"
    mv "$staging_root/tmp/$topdir/prometheus" "$prom_bin_dir/prometheus"
    mv "$staging_root/tmp/$topdir/promtool"   "$prom_bin_dir/promtool"
    chmod 0755 "$prom_bin_dir/prometheus" "$prom_bin_dir/promtool"; rm -rf "$staging_root/tmp/$topdir" "$tb"

    # --- node_exporter (pre-extracted static binary) ---
    if [[ "$goarch" == "loong64" ]]; then
        local staged="${BINS_DIR:+$BINS_DIR/node_exporter/$arch/node_exporter}"
        if [[ -n "$staged" && -f "$staged" ]]; then cp -f "$staged" "$ne_bin_dir/node_exporter"; echo "prebuild: loong64 node_exporter from staged $staged"
        else
            echo "prebuild: loong64 node_exporter not staged -> cross-compiling (assets/build-loong-binaries.sh)"
            MONITOR_LOONG_OUT="$staging_root/tmp" bash "$app_dir/assets/build-loong-binaries.sh" node_exporter \
                || { echo "prebuild: loong64 node_exporter cross-compile FAILED (set MONITOR_BINS_DIR)" >&2; exit 5; }
            cp -f "$staging_root/tmp/node_exporter" "$ne_bin_dir/node_exporter"
        fi
    else
        local staged="${BINS_DIR:+$BINS_DIR/node_exporter/$arch/node_exporter}"
        if [[ -n "$staged" && -f "$staged" ]]; then cp -f "$staged" "$ne_bin_dir/node_exporter"; echo "prebuild: node_exporter from staged $staged"
        else
            local url="https://github.com/prometheus/node_exporter/releases/download/v$NE_VER/node_exporter-$NE_VER.linux-$goarch.tar.gz"
            echo "prebuild: download node_exporter $url"; curl -fL --retry 3 -o "$ne_tb" "$url"
            echo "$(ne_sha "$goarch")  $ne_tb" | sha256sum -c - || { echo "prebuild: node_exporter sha256 MISMATCH ($goarch)" >&2; exit 5; }
            tar xzf "$ne_tb" -C "$staging_root/tmp" "node_exporter-$NE_VER.linux-$goarch/node_exporter"
            mv "$staging_root/tmp/node_exporter-$NE_VER.linux-$goarch/node_exporter" "$ne_bin_dir/node_exporter"
            rm -rf "$staging_root/tmp/node_exporter-$NE_VER.linux-$goarch" "$ne_tb"
        fi
    fi
    chmod 0755 "$ne_bin_dir/node_exporter"
    # scrape config: a `node` job pointing at the in-guest node_exporter on loopback :9100.
    install -Dm0644 "$app_dir/assets/prometheus.yml" "$staging_root/etc/prometheus.yml"
    echo "prebuild: prometheus+promtool+node_exporter staged ($goarch); version red-line $PROM_VER / $NE_VER"
}

install_grafana() {
    # grafana OSS 13.0.1: single fully-static CGO-free Go binary (`grafana server` subcommand) + the
    # frontend SPA in public/ + conf/. amd64/arm64/riscv64 are official dl.grafana.com releases;
    # loong64 upstream ships none -> self-cross-compiled backend + official frontend (staged/recipe).
    local gdir="$staging_root/opt/grafana" topdir="grafana-$GRAFANA_VER" tb="$staging_root/tmp/grafana.tgz"
    mkdir -p "$gdir" "$staging_root/tmp"
    if [[ "$goarch" == "loong64" ]]; then
        local staged="${BINS_DIR:+$BINS_DIR/grafana/$arch/grafana-$GRAFANA_VER.linux-loong64.tar.gz}"
        if [[ -n "$staged" && -f "$staged" ]]; then echo "prebuild: loong64 grafana from staged $staged"; cp -f "$staged" "$tb"
        else
            echo "prebuild: loong64 grafana not staged -> cross-compiling backend + grafting official frontend (assets/build-loong-binaries.sh)"
            MONITOR_LOONG_OUT="$staging_root/tmp" bash "$app_dir/assets/build-loong-binaries.sh" grafana \
                || { echo "prebuild: loong64 grafana build FAILED (set MONITOR_BINS_DIR to a staged artifact)" >&2; exit 6; }
            cp -f "$staging_root/tmp/grafana-$GRAFANA_VER.linux-loong64.tar.gz" "$tb"
        fi
    else
        local url="https://dl.grafana.com/oss/release/grafana-$GRAFANA_VER.linux-$goarch.tar.gz"
        local staged="${BINS_DIR:+$BINS_DIR/grafana/$arch/grafana-$GRAFANA_VER.linux-$goarch.tar.gz}"
        if [[ -n "$staged" && -f "$staged" ]]; then cp -f "$staged" "$tb"; echo "prebuild: grafana from staged $staged"
        else echo "prebuild: download grafana $url"; curl -fL --retry 3 -o "$tb" "$url"; fi
        echo "$(grafana_sha "$goarch")  $tb" | sha256sum -c - || { echo "prebuild: grafana sha256 MISMATCH ($goarch)" >&2; exit 6; }
    fi
    tar xzf "$tb" -C "$staging_root/tmp" "$topdir"
    # bin/ + public/ (frontend SPA) + conf/ (defaults.ini); drop *.map browser debug artifacts
    # (~290MB, never read server-side) and docs/ to keep the overlay lean.
    cp -a "$staging_root/tmp/$topdir/bin"    "$gdir/"
    cp -a "$staging_root/tmp/$topdir/public" "$gdir/"
    cp -a "$staging_root/tmp/$topdir/conf"   "$gdir/"
    find "$gdir/public" -name '*.map' -delete 2>/dev/null || true
    chmod 0755 "$gdir/bin/grafana"
    mkdir -p "$gdir/data" "$gdir/logs" "$gdir/plugins" \
             "$gdir/provisioning/datasources" "$gdir/provisioning/dashboards" \
             "$gdir/provisioning/plugins" "$gdir/provisioning/alerting" \
             "$gdir/provisioning/access-control" "$gdir/provisioning/notifiers"
    rm -rf "$staging_root/tmp/$topdir" "$tb"

    # --- opportunistic BUILD-TIME pre-migration of the arch-independent grafana.db (skips the 709
    #     first-run SQLite migrations on-target). Prefer a staged db, else run the target grafana
    #     (native for x86; qemu-user otherwise), bounded; on any failure ship un-seeded (the carpet
    #     migrates on the spot). grafana.db is arch-independent, so a db built for ANY arch is valid.
    local seeded=0
    local staged_db="${BINS_DIR:+$BINS_DIR/grafana/grafana-$GRAFANA_VER.db}"
    if [[ -n "$staged_db" && -f "$staged_db" ]]; then
        cp -f "$staged_db" "$gdir/data/grafana.db"; seeded=1; echo "prebuild: grafana pre-migrated db from staged $staged_db"
    elif [[ "$GRAFANA_PREMIGRATE" == 1 ]]; then
        premigrate_grafana "$gdir" && seeded=1 || echo "prebuild: grafana pre-migration skipped (on-target will migrate)"
    fi
    echo "prebuild: grafana $GRAFANA_VER staged ($goarch); pre-migrated=$seeded"
}

# run grafana once so the SQLite store migrates, then stop; copy the migrated db as the seed.
premigrate_grafana() {
    # loongarch64: `grafana server` under qemu-loongarch64-static never reaches "HTTP Server Listen"
    # (unlike the amd64 native / aarch64 / riscv64 qemu-user paths), so the build-time seed is skipped
    # and the carpet migrates on-target within its 1200s TCG budget (a stricter, real ext4/fsync test).
    [[ "$goarch" == "loong64" ]] && { echo "prebuild: grafana pre-migration skipped for loong64 (qemu-user grafana server does not listen; on-target migrates)"; return 1; }
    local gdir="$1" port=13300 work="$staging_root/tmp/gfmig"
    rm -rf "$work"; mkdir -p "$work/data" "$work/logs" "$work/plugins" \
        "$work/provisioning/datasources" "$work/provisioning/dashboards" "$work/provisioning/plugins" \
        "$work/provisioning/alerting" "$work/provisioning/access-control" "$work/provisioning/notifiers"
    cat > "$work/grafana.ini" <<INI
app_mode = production
[paths]
data = $work/data
logs = $work/logs
plugins = $work/plugins
provisioning = $work/provisioning
[server]
http_addr = 127.0.0.1
http_port = $port
[database]
type = sqlite3
[analytics]
reporting_enabled = false
check_for_updates = false
check_for_plugin_updates = false
[log]
mode = console
level = error
INI
    local runner=(); [[ "$goarch" != "amd64" ]] && runner=("$qemu_runner" -L "$staging_root")
    echo "prebuild: pre-migrating grafana.db (bounded; ${runner:+via qemu-user})..."
    QEMU_LD_PREFIX="$staging_root" "${runner[@]}" "$gdir/bin/grafana" server \
        --homepath "$gdir" --config "$work/grafana.ini" >"$work/mig.log" 2>&1 &
    local pid=$! up=0 i=0
    while [[ $i -lt 300 ]]; do   # bounded: native/qemu-user grafana that listens comes up well under this
        kill -0 "$pid" 2>/dev/null || break
        if curl -fs --max-time 3 --noproxy '*' "http://127.0.0.1:$port/api/health" >/dev/null 2>&1; then up=1; break; fi
        i=$((i+1)); sleep 1
    done
    kill "$pid" 2>/dev/null; sleep 1; kill -9 "$pid" 2>/dev/null || true
    if [[ $up == 1 && -f "$work/data/grafana.db" ]]; then
        cp -f "$work/data/grafana.db" "$gdir/data/grafana.db"; rm -rf "$work"; return 0
    fi
    rm -rf "$work"; return 1
}

stage_overlay() {
    mkdir -p "$overlay_dir/usr"
    # wholesale /usr closure: glances + python3 + psutil + fastapi/starlette/pydantic + pyte/uvicorn
    # + prometheus/promtool (/usr/local/bin) + node_exporter (/usr/bin) + musl .so closure + terminfo.
    cp -a "$staging_root/usr/bin"        "$overlay_dir/usr/"
    cp -a "$staging_root/usr/lib"        "$overlay_dir/usr/"
    [[ -d "$staging_root/usr/local/bin" ]] && { mkdir -p "$overlay_dir/usr/local"; cp -a "$staging_root/usr/local/bin" "$overlay_dir/usr/local/"; }
    [[ -d "$staging_root/usr/share/terminfo" ]] && { mkdir -p "$overlay_dir/usr/share"; cp -a "$staging_root/usr/share/terminfo" "$overlay_dir/usr/share/"; }
    [[ -d "$staging_root/etc/terminfo" ]] && { mkdir -p "$overlay_dir/etc"; cp -a "$staging_root/etc/terminfo" "$overlay_dir/etc/"; }
    install -Dm0644 "$staging_root/etc/prometheus.yml" "$overlay_dir/etc/prometheus.yml"
    ln -sf python3 "$overlay_dir/usr/bin/python" 2>/dev/null || true
    # grafana homepath (bin/ + public/ + conf/ + pre-migrated data/) under /opt/grafana.
    [[ -d "$staging_root/opt/grafana" ]] && { mkdir -p "$overlay_dir/opt"; cp -a "$staging_root/opt/grafana" "$overlay_dir/opt/"; }

    # carpet suite + pyte harness under /root/monitor; launcher on PATH.
    mkdir -p "$overlay_dir/root/monitor/python" "$overlay_dir/root/monitor/programs"
    local n=0
    for f in "$app_dir"/python/*.py; do install -Dm0644 "$f" "$overlay_dir/root/monitor/python/$(basename "$f")"; n=$((n+1)); done
    install -Dm0644 "$app_dir/programs/pty_tui_drive.py" "$overlay_dir/root/monitor/programs/pty_tui_drive.py"
    install -Dm0644 "$app_dir/programs/pyte_assert.py"   "$overlay_dir/root/monitor/programs/pyte_assert.py"
    install -Dm0755 "$app_dir/programs/run-monitor.sh"   "$overlay_dir/usr/bin/run-monitor.sh"
    local sz; sz=$(du -sm "$overlay_dir" 2>/dev/null | cut -f1)
    echo "prebuild: staged $n python carpet(s) + pyte harness; monitor overlay ready for $arch -- ${sz}M"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
install_glances_closure
vendor_wheel "$PYTE_WHL_URL"    "$PYTE_WHL_SHA"    "pyte-0.8.2"
vendor_wheel "$UVICORN_WHL_URL" "$UVICORN_WHL_SHA" "uvicorn-0.34.0"
install_prometheus_stack
install_grafana
stage_overlay
