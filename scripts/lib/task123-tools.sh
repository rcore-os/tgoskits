#!/usr/bin/env bash

# Resolve external Task 1-3 tools without assuming a developer username or
# home-directory layout. Callers may set the named override; otherwise the
# executable must be discoverable through PATH.
resolve_task123_tool() {
    local override_name="$1"
    local executable_name="$2"
    local configured_path="${!override_name:-}"

    if [[ -n "$configured_path" ]]; then
        if [[ ! -x "$configured_path" ]]; then
            printf 'error: %s is not executable: %s\n' \
                "$override_name" "$configured_path" >&2
            return 1
        fi
        realpath "$configured_path"
        return
    fi

    local discovered_path
    discovered_path="$(command -v "$executable_name" 2>/dev/null || true)"
    if [[ -z "$discovered_path" ]]; then
        printf 'error: required tool %s was not found; set %s or add it to PATH\n' \
            "$executable_name" "$override_name" >&2
        return 1
    fi
    realpath "$discovered_path"
}

resolve_task123_cross_prefix() {
    if [[ -n "${CROSS_COMPILE:-}" ]]; then
        if [[ ! -x "${CROSS_COMPILE}gcc" ]]; then
            printf 'error: CROSS_COMPILE gcc is not executable: %s\n' \
                "${CROSS_COMPILE}gcc" >&2
            return 1
        fi
        printf '%s\n' "$CROSS_COMPILE"
        return
    fi

    local cross_cc
    cross_cc="$(resolve_task123_tool CROSS_CC aarch64-linux-musl-gcc)" || return
    printf '%s\n' "${cross_cc%gcc}"
}
