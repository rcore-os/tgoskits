//! Linux-compatible EL0 ID-register emulation and exposed feature policy.

use ax_cpu::capability::IdRegister;

/// Emulates a supported ID read after copying the instruction through the
/// process-owned user-memory boundary. Unreadable or other instructions retain
/// the existing illegal-instruction path.
pub(super) fn emulate_mrs_id_reg(current: &super::UserTaskRef, context: &mut ax_cpu::user::UserContext) -> bool {
    let Ok(insn) = crate::mm::UserConstPtr::<u32>::from(context.elr as usize).read(current) else {
        return false;
    };
    // MRS (register, read direction): bits[31:20] == 0xD53 and L (bit 21) set.
    if (insn & 0xFFF0_0000) != 0xD530_0000 || (insn & (1 << 21)) == 0 {
        return false;
    }
    let op0 = (insn >> 19) & 0x3;
    let op1 = (insn >> 16) & 0x7;
    let crn = (insn >> 12) & 0xf;
    let crm = (insn >> 8) & 0xf;
    let op2 = (insn >> 5) & 0x7;
    let rt = (insn & 0x1f) as usize;
    // The AArch64 ID feature register space is op0=3, op1=0, CRn=0, CRm=4..7.
    // We must NOT leak the raw EL1 register to EL0 (as Linux's `emulate_mrs`
    // also avoids): the host/QEMU CPU may advertise SVE/SME/MTE/BTI/PAuth/RAS
    // etc. that need kernel context-save/enable flows StarryOS does not have.
    // A program reading those bits would then execute the corresponding
    // instructions and crash or corrupt state. So we expose a sanitized
    // user-safe view: keep only the feature bits whose instructions are plain
    // and stateless (so they Just Work, including under TCG), and report
    // everything else as not-implemented (RAZ).
    // Field masks (each ID field is 4 bits):
    //   PFR0 low 24 bits = EL0/EL1/EL2/EL3/FP/AdvSIMD — baseline, always safe.
    //     Bits >=24 (GIC/RAS/SVE/SEL2/MPAM/AMU) are hidden.
    const PFR0_SAFE: u64 = 0x0000_0000_00FF_FFFF;
    //   ISAR1 PAuth fields APA[7:4] API[11:8] GPA[27:24] GPI[31:28] need kernel
    //   key management; clear them, keep DPB/JSCVT/FCMA/LRCPC/SB/BF16/I8MM/...
    const ISAR1_PAUTH: u64 = 0x0000_0000_FF00_0FF0;
    let val: u64 = match (op0, op1, crn, crm, op2) {
        (3, 0, 0, 4, 0) => IdRegister::Pfr0.read() & PFR0_SAFE,
        (3, 0, 0, 6, 0) => IdRegister::Isar0.read(),
        (3, 0, 0, 6, 1) => IdRegister::Isar1.read() & !ISAR1_PAUTH,
        (3, 0, 0, 7, 0) => IdRegister::Mmfr0.read(),
        (3, 0, 0, 7, 1) => IdRegister::Mmfr1.read(),
        (3, 0, 0, 7, 2) => IdRegister::Mmfr2.read(),
        // Every other ID register in the architectural space — PFR1/PFR2,
        // DFR0/1/2, ZFR0 (SVE), SMFR0 (SME), ISAR2/3 (PAuth/MOPS), MMFR3/4,
        // reserved — describes state-bearing or kernel-only features StarryOS
        // does not implement. Report not-implemented (RAZ) rather than SIGILL,
        // so feature probing degrades to the baseline path instead of crashing.
        (3, 0, 0, 4..=7, _) => 0,
        _ => return false,
    };
    // Rt == 31 encodes XZR; the result is discarded.
    if rt < 31 {
        context.x[rt] = val;
    }
    context.elr += 4;
    true
}
