//! perf side-band records (`PERF_RECORD_COMM` / `MMAP2` / `FORK` / `EXIT`).
//!
//! Sampling gives `perf` raw instruction pointers; to turn those into
//! `function@binary` `perf report` needs the monitored task's executable memory
//! map and name. The kernel supplies that out of band, interleaved into the same
//! mmap ring as the samples, gated by the event's `attr.{comm,mmap2,task}` bits:
//!
//! * `PERF_RECORD_COMM` — the process name, at `execve`.
//! * `PERF_RECORD_MMAP2` — one per executable mapping: the exec image + the
//!   dynamic loader at `execve`, then each shared library as the loader `mmap`s
//!   it. Carries `addr`/`len`/`pgoff`/`filename` so `perf` can map an IP back to
//!   `(binary, file offset)` and read that binary's symbol table.
//! * `PERF_RECORD_FORK` / `EXIT` — task lifetime, at `clone` / `exit`.
//!
//! These are written from *process context* (syscall time), not the IRQ handler,
//! via [`super::sampling::ring_write_process`] (which masks local IRQs to
//! serialize against the overflow handler sharing the ring).
//!
//! ## `sample_id_all`
//!
//! Real `perf record` sets `attr.sample_id_all`, which means *every* record —
//! including these side-band ones — carries a trailing "sample id" section with
//! the `attr.sample_type` subset `{TID, TIME, ID, STREAM_ID, CPU, IDENTIFIER}`.
//! The trailer is part of `header.size`; omitting it would desync `perf`'s parser.
//! [`push_trailer`] appends it when [`SidebandTarget::sample_id_all`] is set.

use alloc::vec::Vec;

/// `PERF_RECORD_COMM`.
const PERF_RECORD_COMM: u32 = 3;
/// `PERF_RECORD_EXIT`.
const PERF_RECORD_EXIT: u32 = 4;
/// `PERF_RECORD_FORK`.
const PERF_RECORD_FORK: u32 = 7;
/// `PERF_RECORD_MMAP2`.
const PERF_RECORD_MMAP2: u32 = 10;
/// `PERF_RECORD_MISC_USER`: the record describes user-space state.
const PERF_RECORD_MISC_USER: u16 = 2;
/// `PERF_RECORD_MISC_COMM_EXEC`: this `COMM` came from `execve` (not `prctl`).
const PERF_RECORD_MISC_COMM_EXEC: u16 = 1 << 13;

// `sample_type` bits relevant to the `sample_id_all` trailer.
const PERF_SAMPLE_TID: u64 = 1 << 1;
const PERF_SAMPLE_TIME: u64 = 1 << 2;
const PERF_SAMPLE_ID: u64 = 1 << 6;
const PERF_SAMPLE_CPU: u64 = 1 << 7;
const PERF_SAMPLE_STREAM_ID: u64 = 1 << 9;
const PERF_SAMPLE_IDENTIFIER: u64 = 1 << 16;

/// `comm` is capped at Linux's `TASK_COMM_LEN` (16, including the NUL).
const COMM_MAX: usize = 15;

/// Where a side-band record is written, plus the parameters of its
/// `sample_id_all` trailer. Built per monitored event from its `PerTaskCounter`.
pub struct SidebandTarget {
    /// Kernel vaddr of the destination ring's header page (`0` ⇒ skip).
    pub ring_vaddr: usize,
    /// Total ring length in bytes.
    pub ring_len: usize,
    /// `attr.sample_type` — selects which fields the trailer carries.
    pub sample_type: u64,
    /// Whether to append the `sample_id_all` trailer at all.
    pub sample_id_all: bool,
    /// Event id (for the trailer's `ID` / `IDENTIFIER` fields).
    pub id: u64,
    /// Process id of the monitored task.
    pub pid: u32,
    /// Thread id of the monitored task.
    pub tid: u32,
}

/// One executable mapping, for [`emit_mmap2`].
pub struct Mmap2Info {
    /// Mapped virtual address.
    pub addr: u64,
    /// Mapping length in bytes.
    pub len: u64,
    /// File offset of the mapping (`pgoff`).
    pub pgoff: u64,
    /// Backing file device major/minor and inode (best-effort; 0 if unknown).
    pub maj: u32,
    pub min: u32,
    pub ino: u64,
    /// Protection + flags (`PROT_*` / `MAP_*`).
    pub prot: u32,
    pub flags: u32,
    /// Backing file path (what `perf` opens to read symbols).
    pub filename: alloc::string::String,
}

#[inline]
fn push_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_ne_bytes());
}
#[inline]
fn push_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_ne_bytes());
}

/// Append a NUL-terminated string padded to an 8-byte boundary (perf record
/// string fields are always 8-aligned).
fn push_cstr_padded(b: &mut Vec<u8>, s: &[u8]) {
    b.extend_from_slice(s);
    b.push(0);
    while b.len() % 8 != 0 {
        b.push(0);
    }
}

/// Append the `sample_id_all` trailer in the canonical order.
fn push_trailer(b: &mut Vec<u8>, t: &SidebandTarget) {
    if !t.sample_id_all {
        return;
    }
    let st = t.sample_type;
    if st & PERF_SAMPLE_TID != 0 {
        push_u32(b, t.pid);
        push_u32(b, t.tid);
    }
    if st & PERF_SAMPLE_TIME != 0 {
        push_u64(b, ax_runtime::hal::time::monotonic_time_nanos());
    }
    if st & PERF_SAMPLE_ID != 0 {
        push_u64(b, t.id);
    }
    if st & PERF_SAMPLE_STREAM_ID != 0 {
        push_u64(b, 0);
    }
    if st & PERF_SAMPLE_CPU != 0 {
        push_u32(b, ax_hal::percpu::this_cpu_id() as u32);
        push_u32(b, 0);
    }
    if st & PERF_SAMPLE_IDENTIFIER != 0 {
        push_u64(b, t.id);
    }
}

/// Back-patch the 8-byte header (reserved at the front of `b`) once the full
/// record length (8-aligned) is known, then write it into the ring.
fn finish_and_write(mut b: Vec<u8>, t: &SidebandTarget, type_: u32, misc: u16) {
    while b.len() % 8 != 0 {
        b.push(0);
    }
    let size = b.len() as u16;
    b[0..4].copy_from_slice(&type_.to_ne_bytes());
    b[4..6].copy_from_slice(&misc.to_ne_bytes());
    b[6..8].copy_from_slice(&size.to_ne_bytes());
    if t.ring_vaddr == 0 {
        return;
    }
    // SAFETY: the caller only builds a target with a non-zero `ring_vaddr` for a
    // ring whose pages are pinned by the owning event for the duration of this
    // call (the monitored task is the running task issuing the syscall, so the
    // ring cannot be torn down concurrently on this single-core path).
    unsafe { super::sampling::ring_write_process(t.ring_vaddr, t.ring_len, &b) };
}

/// Emit a `PERF_RECORD_COMM` for `comm` (truncated to `TASK_COMM_LEN`).
pub fn emit_comm(t: &SidebandTarget, comm: &str, exec: bool) {
    let mut b = Vec::with_capacity(64);
    b.extend_from_slice(&[0u8; 8]); // header placeholder
    push_u32(&mut b, t.pid);
    push_u32(&mut b, t.tid);
    let name = comm.as_bytes();
    push_cstr_padded(&mut b, &name[..name.len().min(COMM_MAX)]);
    push_trailer(&mut b, t);
    let misc = PERF_RECORD_MISC_USER | if exec { PERF_RECORD_MISC_COMM_EXEC } else { 0 };
    finish_and_write(b, t, PERF_RECORD_COMM, misc);
}

/// Emit a `PERF_RECORD_FORK` (`type_` == [`PERF_RECORD_FORK`]) or
/// `PERF_RECORD_EXIT` task-lifetime record. Both carry the same body: the subject
/// task's `pid`/`tid`, its parent's `ppid`/`ptid`, then a `time` stamp.
///
/// The `sample_id_all` trailer reflects the task whose context emits the record
/// (the *parent* for `FORK`, the *exiting task* for `EXIT`) — encoded by the
/// caller in `t.pid`/`t.tid` — matching Linux's `perf_event_header__init_id`.
fn emit_task(t: &SidebandTarget, type_: u32, pid: u32, ppid: u32, tid: u32, ptid: u32) {
    let mut b = Vec::with_capacity(64);
    b.extend_from_slice(&[0u8; 8]); // header placeholder
    push_u32(&mut b, pid);
    push_u32(&mut b, ppid);
    push_u32(&mut b, tid);
    push_u32(&mut b, ptid);
    push_u64(&mut b, ax_runtime::hal::time::monotonic_time_nanos());
    push_trailer(&mut b, t);
    // FORK/EXIT carry no cpu-mode misc bits (the task, not a sampled IP).
    finish_and_write(b, t, type_, 0);
}

/// Emit a `PERF_RECORD_FORK` describing a newly-cloned child (`pid`/`tid`) of the
/// monitored parent (`ppid`/`ptid`). `t` is built in the parent's context.
pub fn emit_fork(t: &SidebandTarget, pid: u32, ppid: u32, tid: u32, ptid: u32) {
    emit_task(t, PERF_RECORD_FORK, pid, ppid, tid, ptid);
}

/// Emit a `PERF_RECORD_EXIT` for the exiting task (`pid`/`tid`) and its parent
/// (`ppid`/`ptid`). `t` is built in the exiting task's context.
pub fn emit_exit(t: &SidebandTarget, pid: u32, ppid: u32, tid: u32, ptid: u32) {
    emit_task(t, PERF_RECORD_EXIT, pid, ppid, tid, ptid);
}

/// Emit a `PERF_RECORD_MMAP2` for one executable mapping.
pub fn emit_mmap2(t: &SidebandTarget, m: &Mmap2Info) {
    let mut b = Vec::with_capacity(128);
    b.extend_from_slice(&[0u8; 8]); // header placeholder
    push_u32(&mut b, t.pid);
    push_u32(&mut b, t.tid);
    push_u64(&mut b, m.addr);
    push_u64(&mut b, m.len);
    push_u64(&mut b, m.pgoff);
    push_u32(&mut b, m.maj);
    push_u32(&mut b, m.min);
    push_u64(&mut b, m.ino);
    push_u64(&mut b, 0); // ino_generation
    push_u32(&mut b, m.prot);
    push_u32(&mut b, m.flags);
    push_cstr_padded(&mut b, m.filename.as_bytes());
    push_trailer(&mut b, t);
    finish_and_write(b, t, PERF_RECORD_MMAP2, PERF_RECORD_MISC_USER);
}
