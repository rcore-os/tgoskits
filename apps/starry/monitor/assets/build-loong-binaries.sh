#!/usr/bin/env bash
# build-loong-binaries.sh -- reproducible LoongArch64 cross-compile of prometheus 3.11.3 + promtool
# and node_exporter 1.11.1 from their pinned upstream source tags. Upstream ships NO loong64
# prebuilt release, and StarryOS runs real loong64 -- so we cross-compile (Go makes this simple),
# NEVER skip the arch. Output tarball/binary land in $MONITOR_LOONG_OUT (default: current dir).
#
# Usage:  build-loong-binaries.sh prometheus     # -> prometheus-3.11.3.linux-loong64.tar.gz
#         build-loong-binaries.sh node_exporter  # -> node_exporter (static loong64 ELF)
#
# Requirements: a Go toolchain >= 1.25 (prometheus/node_exporter go.mod require go 1.25). node_exporter
# is a one-command cross-compile. prometheus additionally needs its web UI embedded (built once with
# Node >= 22, arch-independent) so the binary carries `builtinassets`; if the prebuilt UI embed is
# absent this script builds it. Both are CGO_ENABLED=0 pure-static Go -> no libc needed on StarryOS.
set -euo pipefail
what="${1:?usage: build-loong-binaries.sh prometheus|node_exporter|grafana}"
OUT="${MONITOR_LOONG_OUT:-$(pwd)}"; mkdir -p "$OUT"
PROM_VER="3.11.3"; NE_VER="1.11.1"; GRAFANA_VER="13.0.1"
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
command -v go >/dev/null 2>&1 || { echo "build-loong: need a Go toolchain (>=1.25)"; exit 3; }
export GOTOOLCHAIN="${GOTOOLCHAIN:-auto}" GOWORK=off CGO_ENABLED=0 GOOS=linux GOARCH=loong64
UTS="$(date -u +%Y%m%dT%H:%M:%SZ)"

case "$what" in
  node_exporter)
    echo "=== cross-compile node_exporter $NE_VER (loong64) ==="
    git clone --depth 1 --branch "v$NE_VER" https://github.com/prometheus/node_exporter "$WORK/ne"
    cd "$WORK/ne"; rev="$(git rev-parse HEAD)"
    go build -trimpath \
      -ldflags "-X github.com/prometheus/common/version.Version=$NE_VER \
                -X github.com/prometheus/common/version.Revision=$rev \
                -X github.com/prometheus/common/version.Branch=HEAD \
                -X github.com/prometheus/common/version.BuildUser=rcore-os-tgoskits-crosscompile \
                -X github.com/prometheus/common/version.BuildDate=$UTS" \
      -o "$OUT/node_exporter" .
    file "$OUT/node_exporter" | sed 's/,.*statically/  [static]/'
    echo "=== node_exporter loong64 -> $OUT/node_exporter ==="
    ;;
  prometheus)
    echo "=== cross-compile prometheus $PROM_VER + promtool (loong64, NO embedded web UI) ==="
    # The embedded /graph web UI is deliberately NOT built. Building it needs a Node>=22 + npm/yarn
    # react-scripts frontend build (the `builtinassets` tag); that step is heavy, fragile, and
    # arch-irrelevant, and on Node<22 it fails (assets_embed.go: undefined EmbedFS). The monitor PROM
    # carpet exercises ONLY the API/CLI surface (--version / --help / promtool / /-/ready / PromQL /
    # scrape) -- NOT the web UI -- and the dashboard UI is provided by grafana in this stack. So build
    # the two commands WITHOUT builtinassets: `go build ./cmd/{prometheus,promtool}` yields a fully
    # API-functional prometheus (scrape / TSDB / PromQL / alerting all present; only the built-in
    # /graph HTML page is absent). Still a from-source cross-compile of the official v3.11.3 tag -- NOT
    # a SKIP. (Requires Go>=1.25 per prometheus go.mod; no Node needed.)
    git clone --depth 1 --branch "v$PROM_VER" https://github.com/prometheus/prometheus "$WORK/p"
    cd "$WORK/p"; rev="$(git rev-parse HEAD)"
    local_ld="-X github.com/prometheus/common/version.Version=$PROM_VER \
              -X github.com/prometheus/common/version.Revision=$rev \
              -X github.com/prometheus/common/version.Branch=HEAD \
              -X github.com/prometheus/common/version.BuildUser=rcore-os-tgoskits-crosscompile \
              -X github.com/prometheus/common/version.BuildDate=$UTS"
    mkdir -p "$WORK/stage/prometheus-$PROM_VER.linux-loong64"
    for cmd in prometheus promtool; do
        CGO_ENABLED=0 go build -trimpath -tags netgo -ldflags "$local_ld" \
            -o "$WORK/stage/prometheus-$PROM_VER.linux-loong64/$cmd" "./cmd/$cmd"
    done
    cp -f LICENSE NOTICE documentation/examples/prometheus.yml \
        "$WORK/stage/prometheus-$PROM_VER.linux-loong64/" 2>/dev/null || true
    ( cd "$WORK/stage" && tar czf "$OUT/prometheus-$PROM_VER.linux-loong64.tar.gz" "prometheus-$PROM_VER.linux-loong64" )
    file "$WORK/stage/prometheus-$PROM_VER.linux-loong64/prometheus" | sed 's/,.*/ .../'
    echo "=== prometheus loong64 (no web UI) -> $OUT/prometheus-$PROM_VER.linux-loong64.tar.gz ==="
    ;;
  grafana)
    echo "=== cross-compile grafana $GRAFANA_VER backend (loong64) + graft official frontend ==="
    # grafana v13 backend is a single bin/grafana that does NOT embed the frontend (embed.go only
    # embeds cue.mod schema); the frontend SPA ships separately in the release tar's public/. So we
    # cross-compile ONLY the Go backend and graft the OFFICIAL riscv64 tar's public/+conf/ (both
    # arch-independent) onto it -- no Node/yarn frontend build needed for loong64.
    git clone --depth 1 --branch "v$GRAFANA_VER" https://github.com/grafana/grafana "$WORK/g"
    cd "$WORK/g"; rev="$(git rev-parse HEAD)"
    # No GOSUMDB=off here: grafana v13's go.mod pins `toolchain go1.25.9`, and GOTOOLCHAIN=auto must
    # be able to fetch+verify that toolchain (prometheus/node_exporter build without GOSUMDB=off too).
    go build -buildvcs=false -trimpath \
      -ldflags "-X main.version=$GRAFANA_VER -X main.commit=$rev -X main.buildBranch=HEAD -X main.buildstamp=$(date -u +%s)" \
      -o "$WORK/bin/grafana" ./pkg/cmd/grafana
    file "$WORK/bin/grafana" | sed 's/,.*/ .../'
    # graft onto the official riscv64 release tar (public/ + conf/ are arch-independent).
    local_rv="$WORK/grafana-$GRAFANA_VER.linux-riscv64.tar.gz"
    curl -fL --retry 3 -o "$local_rv" "https://dl.grafana.com/oss/release/grafana-$GRAFANA_VER.linux-riscv64.tar.gz"
    tar xzf "$local_rv" -C "$WORK"
    cp -f "$WORK/bin/grafana" "$WORK/grafana-$GRAFANA_VER/bin/grafana"
    # Keep the archive's top dir as `grafana-$GRAFANA_VER` (matching the official tars + what
    # prebuild.sh extracts, topdir="grafana-$GRAFANA_VER"); only the output filename carries the arch.
    ( cd "$WORK" && tar czf "$OUT/grafana-$GRAFANA_VER.linux-loong64.tar.gz" "grafana-$GRAFANA_VER" )
    echo "=== grafana loong64 -> $OUT/grafana-$GRAFANA_VER.linux-loong64.tar.gz ==="
    ;;
  *) echo "unknown target: $what"; exit 2 ;;
esac
