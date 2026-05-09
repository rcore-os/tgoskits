#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"
CONFIG_FILE="$PLUGIN_ROOT/config/docker-ci.toml"
CACHE_DIR="$PLUGIN_ROOT/cache"
WORKSPACE="$(git rev-parse --show-toplevel)"

mkdir -p "$CACHE_DIR"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
die() { echo "ERROR: $*" >&2; exit 1; }
warn() { echo "WARN: $*" >&2; }

toml_get() {
    local key="$1" section="" line
    while IFS= read -r line; do
        case "$line" in
            \[*\]) section="${line//\[/}"; section="${section//\]/}" ;;
            *=*)
                local k="${line%%=*}" v="${line#*=}"
                k="${k// /}"; v="${v// /}"; v="${v//\"/}"
                if [ "$key" = "$section.$k" ]; then echo "$v"; return 0; fi
                ;;
        esac
    done
    return 1
}

image_exists() {
    docker image inspect "$1" >/dev/null 2>&1
}

compute_hash() {
    local img="$1"
    local triggers section="${img}_image"
    # Extract the rebuild_triggers array value (everything between the brackets)
    triggers=$(sed -n "/\[${section}\]/,/^\[/p" "$CONFIG_FILE" \
        | grep "rebuild_triggers" \
        | sed 's/.*\[\(.*\)\].*/\1/' \
        | tr ',' '\n' \
        | sed 's/"//g;s/^ *//;s/ *$//')
    local hash_input=""
    while IFS= read -r f; do
        [ -n "$f" ] && [ -f "$WORKSPACE/$f" ] && hash_input+=$(sha256sum "$WORKSPACE/$f")
    done <<< "$triggers"
    if [ -z "$hash_input" ]; then
        echo "0000000000000000000000000000000000000000000000000000000000000000"
    else
        echo "$hash_input" | sha256sum | cut -d' ' -f1
    fi
}

remote_exists() {
    docker manifest inspect "$1" >/dev/null 2>&1
}

push_image() {
    local name="$1" remote="$2"
    if [ -z "${GITHUB_TOKEN:-}" ] && [ -z "${CR_PAT:-}" ]; then
        warn "No GITHUB_TOKEN or CR_PAT set. Skipping push of $name to $remote."
        return 0
    fi
    echo "$GITHUB_TOKEN" | docker login ghcr.io -u seek-hope --password-stdin 2>/dev/null || true
    docker tag "$name" "$remote"
    docker push "$remote"
    echo "Pushed $name -> $remote"
}

# ---------------------------------------------------------------------------
# Image management
# ---------------------------------------------------------------------------
ensure_image() {
    local img_section="${1}_image" img_tag="$1"
    local name remote hash_file hash dockerfile

    name=$(toml_get "${img_section}.name" < "$CONFIG_FILE")
    remote=$(toml_get "${img_section}.remote" < "$CONFIG_FILE")
    dockerfile=$(toml_get "${img_section}.dockerfile" < "$CONFIG_FILE")
    hash_file="$CACHE_DIR/docker-image-${img_tag}.hash"
    hash=$(compute_hash "$img_tag")

    # Rebuild if trigger files changed
    if [ -f "$hash_file" ] && [ "$(cat "$hash_file")" != "$hash" ]; then
        echo "[$name] Trigger files changed, rebuilding..."
        if docker build -t "$name" -f "$WORKSPACE/$dockerfile" "$WORKSPACE" --cache-from "$name"; then
            echo "$hash" > "$hash_file"
            push_image "$name" "$remote"
        else
            warn "Build failed, keeping existing local image"
        fi
        return 0
    fi

    # Local image exists
    if image_exists "$name"; then
        echo "[$name] Using local image"
        return 0
    fi

    # No local image -> build
    echo "[$name] No local image, building from $dockerfile..."
    if docker build -t "$name" -f "$WORKSPACE/$dockerfile" "$WORKSPACE"; then
        echo "$hash" > "$hash_file"
        if remote_exists "$remote"; then
            local remote_hash
            remote_hash=$(docker manifest inspect "$remote" 2>/dev/null | grep -o '"digest":"[^"]*"' | head -1 | cut -d'"' -f4)
            if [ -n "$remote_hash" ] && [ "$hash" != "$remote_hash" ]; then
                echo "[$name] Remote differs from local, pushing..."
                push_image "$name" "$remote"
            else
                echo "[$name] Remote matches local, no push needed"
            fi
        else
            echo "[$name] No remote image found, pushing..."
            push_image "$name" "$remote"
        fi
    else
        warn "Build failed, falling back to remote..."
        if remote_exists "$remote"; then
            docker pull "$remote"
            docker tag "$remote" "$name"
        else
            die "Cannot build or pull $name. Aborting."
        fi
    fi
}

# ---------------------------------------------------------------------------
# Running commands
# ---------------------------------------------------------------------------
run_in_container() {
    local image="$1" cmd="$2"
    echo "=== [$image] $cmd ==="
    docker run --rm -v "$WORKSPACE:/workspace" -w /workspace "$image" bash -c "$cmd"
}

# ---------------------------------------------------------------------------
# Main dispatch
# ---------------------------------------------------------------------------
cmd_quick() {
    ensure_image "base"
    local commands line
    while IFS= read -r line; do
        line=$(echo "$line" | sed 's/^[[:space:]]*"//;s/",\?$//')
        [ -z "$line" ] && continue
        run_in_container "tgoskits-ci" "$line" || { echo "FAIL: $line"; return 1; }
    done < <(sed -n '/\[quick\]/,/^\[/p' "$CONFIG_FILE" | grep '^\s*"')
    echo "ALL QUICK CHECKS PASSED"
}

cmd_full() {
    ensure_image "base"
    ensure_image "axvisor_lvz"

    local failed=0 line
    while IFS= read -r line; do
        line=$(echo "$line" | sed 's/^[[:space:]]*"//;s/",\?$//')
        [ -z "$line" ] && continue
        local img="tgoskits-ci"
        if echo "$line" | grep -q "axvisor.*loongarch64"; then
            img="tgoskits-ci-lvz"
        fi
        run_in_container "$img" "$line" || { echo "FAIL: $line"; failed=1; }
    done < <(sed -n '/\[full\]/,/^\[/p' "$CONFIG_FILE" | grep '^\s*"')

    if [ "$failed" -eq 0 ]; then
        echo "ALL FULL CI CHECKS PASSED"
    else
        echo "SOME CHECKS FAILED"
        return 1
    fi
}

cmd_test() {
    local os="$1" arch="$2"
    if [ -z "$os" ] || [ -z "$arch" ]; then
        die "Usage: local-ci.sh test <os> <arch>"
    fi
    ensure_image "base"
    local img="tgoskits-ci"
    local cmd="cargo xtask ${os} test qemu --arch ${arch}"
    if [ "$os" = "axvisor" ] && [ "$arch" = "loongarch64" ]; then
        ensure_image "axvisor_lvz"
        img="tgoskits-ci-lvz"
    fi
    run_in_container "$img" "$cmd"
}

cmd_rebuild() {
    local push="${1:-}"
    local hash dockerfile

    echo "Rebuilding base image..."
    dockerfile=$(toml_get "base_image.dockerfile" < "$CONFIG_FILE")
    hash=$(compute_hash "base")
    docker build -t tgoskits-ci -f "$WORKSPACE/$dockerfile" "$WORKSPACE" --no-cache
    echo "$hash" > "$CACHE_DIR/docker-image-base.hash"

    echo "Rebuilding axvisor-lvz image..."
    dockerfile=$(toml_get "axvisor_lvz_image.dockerfile" < "$CONFIG_FILE")
    hash=$(compute_hash "axvisor_lvz")
    docker build -t tgoskits-ci-lvz -f "$WORKSPACE/$dockerfile" "$WORKSPACE" --no-cache
    echo "$hash" > "$CACHE_DIR/docker-image-lvz.hash"

    echo "Validating base image..."
    run_in_container "tgoskits-ci" "cargo xtask arceos qemu --package ax-helloworld --arch aarch64" \
        || die "Base image validation failed"

    if [ "$push" = "--push" ]; then
        local base_remote lvz_remote
        base_remote=$(toml_get "base_image.remote" < "$CONFIG_FILE")
        lvz_remote=$(toml_get "axvisor_lvz_image.remote" < "$CONFIG_FILE")
        push_image "tgoskits-ci" "$base_remote"
        push_image "tgoskits-ci-lvz" "$lvz_remote"
    fi
    echo "Rebuild complete"
}

# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
usage() {
    cat <<'EOF'
Usage: local-ci.sh <command>

Commands:
  full                 Run full CI matrix (all arches, all OSes)
  quick                Run quick checks (fmt + clippy + sync-lint)
  test <os> <arch>     Run single-arch QEMU test
  rebuild              Force rebuild both Docker images
  rebuild --push       Force rebuild + validate + push to remote

Examples:
  ./scripts/local-ci.sh quick
  ./scripts/local-ci.sh test starry aarch64
  ./scripts/local-ci.sh full
  ./scripts/local-ci.sh rebuild --push
EOF
}

case "${1:-}" in
    full)      cmd_full ;;
    quick)     cmd_quick ;;
    test)      cmd_test "$2" "$3" ;;
    rebuild)   cmd_rebuild "${2:-}" ;;
    *)         usage; exit 1 ;;
esac
