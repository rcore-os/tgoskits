//! vCPU presentation helpers.
//!
//! The control plane reports one vCPU's CPU affinity as AxVisor's
//! `phys_cpu_set: Option<usize>` (`virtualization/axvm/src/vcpu.rs`), i.e. a
//! **bitmask**: `phys_cpu_ids = [1]` becomes `0b10` (`virtualization/axvm/src/config.rs`
//! documents the mapping) and `None` becomes `null`. Everything here works on
//! that mask — the panel must never treat it as a list of CPU ids.

/**
 * CPU ids selected by an affinity mask, ascending; `[]` for "not pinned".
 *
 * Bit `n` set means Core `n`. The loop divides by powers of two instead of
 * using `>>`/`&`: JavaScript bitwise operators truncate to 32 bits, and a host
 * with more than 32 CPUs would silently lose the higher bits.
 */
export function decodeCpuSet(mask: number | null | undefined): number[] {
  if (mask === null || mask === undefined || mask === 0) return []
  const cpus: number[] = []
  for (let bit = 0; bit < 64; bit += 1) {
    if (Math.floor(mask / 2 ** bit) % 2 === 1) cpus.push(bit)
  }
  return cpus
}

/** Operator-facing affinity text, e.g. `Core 1（掩码 0x2）` or `未绑定`. */
export function describeCpuAffinity(mask: number | null | undefined): string {
  const cpus = decodeCpuSet(mask)
  if (cpus.length === 0) return '未绑定'
  return `Core ${cpus.join(',')}（掩码 0x${(mask ?? 0).toString(16)}）`
}
