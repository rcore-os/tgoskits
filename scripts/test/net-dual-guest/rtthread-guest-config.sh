# Shared guest-configuration invariants for the RT-Thread dual-guest images.
#
# Every RT-Thread guest in this directory is built from the same upstream
# `qemu-virt64-aarch64` BSP, whose stock `.config` enables the GICv3 ITS. The
# AxVisor vGIC presents no ITS to a guest, so that driver dereferences a null
# redistributor base and takes a data abort during `rt_hw_board_init`:
#
#     pic-gicv3-its.c:405  gicr_supports_plpis()
#     ESR=0x96000005 FAR=0x8  ->  rt_hw_cpu_shutdown() -> RT_ASSERT(0)
#
# The guest then spins in the assert handler before its console comes up, which
# reads from the host side like a scheduler or timer fault rather than a guest
# crash. Each guest disables the option in its own config patch because the
# rest of those patches legitimately differ; this file holds the one rule they
# must all satisfy so a new guest cannot silently inherit the BSP default.
#
# Source this file from a build script and call the assertion after every
# config patch has been applied.

# Fails the build when a guest `.config` would initialize a GICv3 ITS.
#
# $1: path to the BSP `.config` produced after all config patches are applied.
assert_guest_config_has_no_gicv3_its() {
    local config_path="$1"

    if [[ ! -f "$config_path" ]]; then
        printf 'error: guest .config not found: %s\n' "$config_path" >&2
        return 1
    fi
    if grep -qx 'CONFIG_RT_PIC_ARM_GIC_V3_ITS=y' "$config_path"; then
        printf 'error: %s enables CONFIG_RT_PIC_ARM_GIC_V3_ITS\n' "$config_path" >&2
        printf 'error: the AxVisor vGIC exposes no ITS; the guest would abort in\n' >&2
        printf 'error: gicr_supports_plpis() before reaching its console\n' >&2
        return 1
    fi
}

# Makes a pinned commit checkoutable from a `--shared` clone of the cache.
#
# The cache is a partial clone (`--filter=blob:none`), so its blobs are fetched
# lazily through its promisor remote. A `git clone --shared` build tree does not
# inherit that promisor, so checking the commit out there fails with a wall of
#
#     error: unable to read sha1 file of tools/utils.py (...)
#
# and then `bsp/qemu-virt64-aarch64/.config: No such file or directory`. The
# failure only appears against a freshly created cache; once any checkout has
# materialized the blobs the build works, which is why it can stay hidden for a
# long time on a developer machine that has built before.
#
# Checking the commit out inside the cache itself backfills exactly the blobs
# that commit needs, using the cache's own promisor. It is idempotent, so the
# cost is paid once per pinned commit.
#
# $1: path to the RT-Thread cache checkout.
# $2: pinned commit to make available.
ensure_rtthread_commit_is_materialized() {
    local cache_path="$1" commit="$2"

    if ! git -C "$cache_path" cat-file -e "$commit^{commit}" 2>/dev/null; then
        git -C "$cache_path" fetch --quiet origin "$commit"
    fi
    git -C "$cache_path" checkout --quiet --detach "$commit"
}
