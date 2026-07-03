use alloc::{
    borrow::Cow,
    boxed::Box,
    format,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    ffi::CStr,
    fmt::Write,
    iter,
    mem::size_of,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_lazyinit::LazyInit;
use ax_memory_addr::{MemoryAddr, VirtAddr};
#[cfg(target_arch = "aarch64")]
use ax_runtime::hal::pmu;
use ax_runtime::hal::{
    paging::MappingFlags,
    time::{monotonic_time, wall_time},
};
use ax_task::{AxCpuMask, AxTaskRef, TaskState, WeakAxTaskRef, current};
use axfs_ng_vfs::{DeviceId, Filesystem, NodePermission, NodeType, VfsError, VfsResult};
use kernel_elf_parser::{AuxEntry, AuxType};
use ksym::KallsymsMapped;
use starry_process::{Pid, Process};
use zerocopy::IntoBytes;

use crate::{
    file::FD_TABLE,
    mm::{BackendFileInfo, ProcessMemStats},
    pseudofs::{
        DirMaker, DirMapping, DirectRwFsFileOps, NodeOpsMux, RwFile, SeqObject, SimpleDir,
        SimpleDirOps, SimpleFile, SimpleFileOperation, SimpleFs, SpecialFsFile,
    },
    task::{
        AsThread, Cred, ProcessData, TaskStat, Thread, get_process_data, get_task, processes,
        tasks, tick_cpu_time,
    },
};

/// Global IRQ counter incremented on every timer tick.
/// Module-level so both `/proc/interrupts` and `/proc/stat` can read it.
static IRQ_CNT: AtomicUsize = AtomicUsize::new(0);
const PROCFS_INIT_PID: Pid = 1;

pub static KALLSYMS: LazyInit<KallsymsMapped<'static>> = LazyInit::new();

fn read_kallsyms() -> KallsymsMapped<'static> {
    unsafe extern "C" {
        fn _stext();
        fn _etext();
        fn __kallsyms_start();
        fn __kallsyms_end();
    }

    let kallsyms_start = __kallsyms_start as *const () as usize;
    let kallsyms_end = __kallsyms_end as *const () as usize;
    let kallsyms_sec_size = kallsyms_end - kallsyms_start;
    let kallsyms_sec =
        unsafe { core::slice::from_raw_parts(__kallsyms_start as *const u8, kallsyms_sec_size) };

    let total_size =
        KallsymsMapped::check_total_bytes(kallsyms_sec).expect("Invalid kallsyms format");

    let kallsyms = &kallsyms_sec[..total_size as usize];
    // TODO: recycle unused space in .kallsyms section
    info!("Read kallsyms, size: {}KB", kallsyms.len() / 1024);
    KallsymsMapped::from_blob(
        kallsyms,
        _stext as *const () as u64,
        _etext as *const () as u64,
    )
    .expect("Failed to create KallsymsMapped")
}

fn procfs_visible_pid(proc: &Arc<Process>) -> Pid {
    if proc.is_init() {
        PROCFS_INIT_PID
    } else {
        proc.pid()
    }
}

fn procfs_lookup_process(pid: Pid) -> VfsResult<Arc<ProcessData>> {
    if pid == PROCFS_INIT_PID {
        processes()
            .into_iter()
            .find(|proc_data| proc_data.proc.is_init())
            .ok_or(VfsError::NotFound)
    } else {
        get_process_data(pid).map_err(|_| VfsError::NotFound)
    }
}

fn render_meminfo() -> String {
    let total = ax_runtime::hal::mem::total_ram_size();
    let usages = ax_alloc::global_allocator().usages();
    // Sum all allocator categories to estimate kernel-consumed memory.
    let used = usages.get(ax_alloc::UsageKind::RustHeap)
        + usages.get(ax_alloc::UsageKind::VirtMem)
        + usages.get(ax_alloc::UsageKind::PageCache)
        + usages.get(ax_alloc::UsageKind::PageTable)
        + usages.get(ax_alloc::UsageKind::Dma)
        + usages.get(ax_alloc::UsageKind::Global);
    let cached = usages.get(ax_alloc::UsageKind::PageCache);
    let page_tables = usages.get(ax_alloc::UsageKind::PageTable);
    let anon_pages = usages.get(ax_alloc::UsageKind::VirtMem);
    let free = total.saturating_sub(used);

    let total_kb = total / 1024;
    let free_kb = free / 1024;
    let cached_kb = cached / 1024;
    let available_kb = free_kb + cached_kb;
    let page_tables_kb = page_tables / 1024;
    let anon_pages_kb = anon_pages / 1024;

    format!(
        "MemTotal:       {total_kb:>10} kB\n\
         MemFree:        {free_kb:>10} kB\n\
         MemAvailable:   {available_kb:>10} kB\n\
         Buffers:                 0 kB\n\
         Cached:         {cached_kb:>10} kB\n\
         SwapCached:              0 kB\n\
         SwapTotal:               0 kB\n\
         SwapFree:                0 kB\n\
         Dirty:                   0 kB\n\
         Writeback:               0 kB\n\
         AnonPages:      {anon_pages_kb:>10} kB\n\
         Mapped:                  0 kB\n\
         Shmem:                   0 kB\n\
         KReclaimable:            0 kB\n\
         Slab:                    0 kB\n\
         SReclaimable:            0 kB\n\
         SUnreclaim:              0 kB\n\
         KernelStack:             0 kB\n\
         PageTables:     {page_tables_kb:>10} kB\n\
         NFS_Unstable:            0 kB\n\
         Bounce:                  0 kB\n\
         WritebackTmp:            0 kB\n\
         CommitLimit:    {total_kb:>10} kB\n\
         Committed_AS:            0 kB\n\
         VmallocTotal:   34359738367 kB\n\
         VmallocUsed:             0 kB\n\
         VmallocChunk:            0 kB\n\
         HugePages_Total:         0\n\
         HugePages_Free:          0\n\
         Hugepagesize:         2048 kB\n"
    )
}

fn render_cpuinfo() -> String {
    let cpu_count = ax_runtime::hal::cpu_num();
    let mut buf = String::new();
    for i in 0..cpu_count {
        render_cpu_entry(&mut buf, i);
    }
    buf
}

/// Read a root-node device-tree property as its raw bytes (NUL-separated,
/// NUL-terminated string list), exactly as Linux exposes under
/// `/proc/device-tree/`. Returns `None` when the FDT is unavailable (non-FDT
/// platform) or the property is missing/empty. Only the JPU/MPP path consumes
/// this, so it is gated on the `jpeg` feature.
#[cfg(all(feature = "jpeg", feature = "plat-dyn"))]
fn read_dt_root_property(name: &str) -> Option<Vec<u8>> {
    rdrive::with_fdt(|fdt| {
        let root = fdt.get_by_path("/")?;
        let prop = root.as_node().get_property(name)?;
        (!prop.data.is_empty()).then(|| prop.data.clone())
    })
    .flatten()
    .map(|mut bytes| {
        // Real OF always NUL-terminates; guard a malformed blob. Do NOT flatten
        // interior NULs — consumers (e.g. librockchip_mpp) expect raw bytes.
        if bytes.last() != Some(&0) {
            bytes.push(0);
        }
        bytes
    })
}

#[cfg(all(feature = "jpeg", not(feature = "plat-dyn")))]
fn read_dt_root_property(_name: &str) -> Option<Vec<u8>> {
    None
}

#[cfg(target_arch = "riscv64")]
fn render_cpu_entry(buf: &mut String, idx: usize) {
    let _ = writeln!(buf, "processor\t: {idx}");
    let _ = writeln!(buf, "hart\t\t: {idx}");
    let _ = writeln!(buf, "isa\t\t: rv64imafdc_zicsr_zifencei");
    let _ = writeln!(buf, "mmu\t\t: sv39");
    let _ = writeln!(buf); // blank line between processors
}

#[cfg(target_arch = "aarch64")]
fn render_cpu_entry(buf: &mut String, idx: usize) {
    // Decode MIDR_EL1 so /proc/cpuinfo reflects the real core (perf and other
    // tools key off implementer/part to identify the microarchitecture). On
    // RK3588 this yields A76 (0x41/0xd0b) and A55 (0x41/0xd05); under QEMU
    // cortex-a53 it reads 0x41/0xd03.
    let midr = pmu::cpu_id_raw().unwrap_or(0);
    let implementer = (midr >> 24) & 0xff;
    let variant = (midr >> 20) & 0xf;
    let part = (midr >> 4) & 0xfff;
    let revision = midr & 0xf;

    let _ = writeln!(buf, "processor\t: {idx}");
    let _ = writeln!(buf, "BogoMIPS\t: 100.00");
    let _ = writeln!(buf, "CPU implementer\t: {implementer:#04x}");
    let _ = writeln!(buf, "CPU architecture: 8");
    let _ = writeln!(buf, "CPU variant\t: {variant:#x}");
    let _ = writeln!(buf, "CPU part\t: {part:#05x}");
    let _ = writeln!(buf, "CPU revision\t: {revision}");
    let _ = writeln!(buf);
}

#[cfg(target_arch = "x86_64")]
fn render_cpu_entry(buf: &mut String, idx: usize) {
    let _ = writeln!(buf, "processor\t: {idx}");
    let _ = writeln!(buf, "vendor_id\t: GenuineIntel");
    let _ = writeln!(buf, "cpu family\t: 6");
    let _ = writeln!(buf, "model\t\t: 85");
    let _ = writeln!(buf, "model name\t: QEMU Virtual CPU v2.5+");
    let _ = writeln!(buf, "stepping\t: 0");
    let _ = writeln!(
        buf,
        "flags\t\t: fpu de pse tsc msr pae mce cx8 apic sep mtrr pge mca cmov pat pse36 clflush \
         mmx fxsr sse sse2 ht syscall nx lm constant_tsc"
    );
    let _ = writeln!(buf);
}

#[cfg(target_arch = "loongarch64")]
fn render_cpu_entry(buf: &mut String, idx: usize) {
    let _ = writeln!(buf, "processor\t\t: {idx}");
    let _ = writeln!(buf, "core id\t\t\t: {idx}");
    let _ = writeln!(buf, "Virtual Machine\t\t: no");
    let _ = writeln!(buf, "Model Name\t\t: QEMU Virtual Machine");
    let _ = writeln!(buf, "ISA\t\t\t: loongarch32 loongarch64");
    let _ = writeln!(
        buf,
        "Feat\t\t\t: cpucfg lam ual fpu lsx lasx crc32 complex crypto lvz"
    );
    let _ = writeln!(buf);
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "x86_64",
    target_arch = "loongarch64"
)))]
fn render_cpu_entry(buf: &mut String, idx: usize) {
    let _ = writeln!(buf, "processor\t: {idx}");
    let _ = writeln!(buf);
}

fn render_stat() -> String {
    let up = monotonic_time();
    let cpu_count = ax_runtime::hal::cpu_num() as u64;
    // Total CPU-time budget in jiffies across all CPUs (USER_HZ = 100).
    let up_jiffies = up.as_secs() * 100 + (up.subsec_millis() / 10) as u64;
    let total_budget = up_jiffies.saturating_mul(cpu_count);

    // Single snapshot: aggregate CPU time and count task states together
    // to avoid holding the task-table lock twice and getting inconsistent data.
    let all_tasks = tasks();
    let mut user_ms: u128 = 0;
    let mut sys_ms: u128 = 0;
    let mut procs_running: u64 = 0;
    let mut procs_blocked: u64 = 0;
    for task in &all_tasks {
        let (u, s) = crate::task::task_cpu_time(task);
        user_ms += u.as_millis();
        sys_ms += s.as_millis();
        match task.state() {
            TaskState::Running | TaskState::Ready => procs_running += 1,
            TaskState::Blocked => procs_blocked += 1,
            TaskState::Exited => {}
        }
    }
    let task_count = all_tasks.len() as u64;
    // 1 jiffy = 10 ms
    let user_jiffies = (user_ms / 10) as u64;
    let sys_jiffies = (sys_ms / 10) as u64;
    let idle_jiffies = total_budget
        .saturating_sub(user_jiffies)
        .saturating_sub(sys_jiffies);
    let procs_running = procs_running.max(1); // at least the current task

    // btime = Unix boot timestamp = wall_clock_now − monotonic_uptime.
    let btime = wall_time().as_secs().saturating_sub(up.as_secs());

    // Per-CPU lines: divide aggregate time evenly (no per-CPU tracking yet).
    let per_cpu_user = user_jiffies / cpu_count;
    let per_cpu_sys = sys_jiffies / cpu_count;
    let per_cpu_idle = idle_jiffies / cpu_count;

    let irq_total = IRQ_CNT.load(Ordering::Relaxed) as u64;

    let mut buf = format!("cpu  {user_jiffies} 0 {sys_jiffies} {idle_jiffies} 0 0 0 0 0 0\n");
    for i in 0..cpu_count {
        let _ = writeln!(
            buf,
            "cpu{i} {per_cpu_user} 0 {per_cpu_sys} {per_cpu_idle} 0 0 0 0 0 0"
        );
    }
    let _ = writeln!(buf, "intr {irq_total}");
    let _ = writeln!(buf, "ctxt 0");
    let _ = writeln!(buf, "btime {btime}");
    let _ = writeln!(buf, "processes {task_count}");
    let _ = writeln!(buf, "procs_running {procs_running}");
    let _ = writeln!(buf, "procs_blocked {procs_blocked}");
    let _ = writeln!(buf, "softirq 0 0 0 0 0 0 0 0 0 0 0");
    buf
}

fn render_proc_net_arp() -> String {
    let mut entries = ax_net::arp_entries();
    entries.sort_by(|a, b| {
        a.device
            .cmp(&b.device)
            .then_with(|| a.ip_addr.cmp(&b.ip_addr))
    });

    let mut buf = "IP address       HW type     Flags       HW address            Mask     \
                   Device\n"
        .to_string();
    for entry in entries {
        let ip = entry.ip_addr;
        let mac = entry.hw_addr;
        let ip_addr = format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
        let _ = writeln!(
            buf,
            "{:<16} 0x{:<8x} 0x{:<8x} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}     *        {}",
            ip_addr,
            entry.hw_type,
            entry.flags,
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
            entry.device
        );
    }
    buf
}

fn render_proc_net_dev() -> String {
    let mut buf = "Inter-|   Receive                                                |  \
                   Transmit\nface |bytes    packets errs drop fifo frame compressed \
                   multicast|bytes    packets errs drop fifo colls carrier compressed\n"
        .to_string();
    for iface in ax_net::interfaces() {
        let _ = writeln!(
            buf,
            "{:>8}:       0       0    0    0    0     0          0         0        0       0    \
             0    0    0     0       0          0",
            iface.name
        );
    }
    buf
}

pub fn new_procfs() -> Filesystem {
    SimpleFs::new_with("proc".into(), 0x9fa0, builder)
}

struct ProcessTaskDir {
    fs: Arc<SimpleFs>,
    process: Weak<Process>,
}

impl SimpleDirOps for ProcessTaskDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let Some(process) = self.process.upgrade() else {
            return Box::new(iter::empty());
        };
        Box::new(
            process
                .threads()
                .into_iter()
                .map(|tid| tid.to_string().into()),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let process = self.process.upgrade().ok_or(VfsError::NotFound)?;
        let tid = name.parse::<u32>().map_err(|_| VfsError::NotFound)?;
        let task = get_task(tid).map_err(|_| VfsError::NotFound)?;
        if task.as_thread().proc_data.proc.pid() != process.pid() {
            return Err(VfsError::NotFound);
        }

        let proc_data = get_process_data(process.pid()).map_err(|_| VfsError::NotFound)?;

        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            self.fs.clone(),
            Arc::new(ThreadDir {
                fs: self.fs.clone(),
                task: Arc::downgrade(&task),
                proc_data,
                path_pid: process.pid(),
                procfs_pid: None,
            }),
        )))
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

/// Render `/proc/[pid]/status` from a live task and authoritative process lookup.
///
/// `path_pid` is the numeric directory name (`/proc/<path_pid>/status`), used for
/// memory counters so cross-process reads do not depend on a possibly stale task
/// weak reference after pid reuse.
fn render_thread_status(
    task: &WeakAxTaskRef,
    proc_data: &Arc<ProcessData>,
    path_pid: Pid,
    procfs_pid: Option<Pid>,
) -> VfsResult<String> {
    let task = task.upgrade().ok_or(VfsError::NotFound)?;
    let thread = task.as_thread();
    let aspace_arc = proc_data.aspace();
    let mem = ProcessMemStats::collect(&aspace_arc.lock());
    let cred = thread.cred();
    let name = task.name();
    let num_threads = proc_data.proc.threads().len() as u32;
    let tracer_pid = proc_data.ptrace_tracer_pid().unwrap_or(0);
    let ppid = proc_data.proc.parent().map_or(0, |parent| parent.pid());
    let (tgid, pid) = if let Some(pid) = procfs_pid {
        (pid, pid as u64)
    } else {
        (path_pid, thread.tid() as u64)
    };
    Ok(render_task_status(
        TaskStatusBase {
            name: &name,
            state: task_status_state(&task),
            tgid,
            pid,
            ppid,
            tracer_pid,
            cred: &cred,
            num_threads,
        },
        task.cpumask(),
        ax_runtime::hal::cpu_num(),
        &mem,
    ))
}

fn task_status_state(task: &AxTaskRef) -> &'static str {
    match task.state() {
        TaskState::Running | TaskState::Ready => "R (running)",
        TaskState::Blocked => "S (sleeping)",
        TaskState::Exited => "Z (zombie)",
    }
}

struct TaskStatusBase<'a> {
    name: &'a str,
    state: &'a str,
    tgid: u32,
    pid: u64,
    ppid: u32,
    tracer_pid: u32,
    cred: &'a crate::task::Cred,
    num_threads: u32,
}

struct TaskStatusFields<'a> {
    base: TaskStatusBase<'a>,
    cpus_allowed: &'a str,
    cpus_allowed_list: &'a str,
    mem: &'a ProcessMemStats,
}

fn render_task_status(
    base: TaskStatusBase<'_>,
    cpumask: AxCpuMask,
    cpu_num: usize,
    mem: &ProcessMemStats,
) -> String {
    let cpus_allowed = format_cpumask_hex(cpumask, cpu_num);
    let cpus_allowed_list = format_cpumask_list(cpumask, cpu_num);

    render_task_status_fields(&TaskStatusFields {
        base,
        cpus_allowed: &cpus_allowed,
        cpus_allowed_list: &cpus_allowed_list,
        mem,
    })
}

#[rustfmt::skip]
fn render_task_status_fields(status: &TaskStatusFields<'_>) -> String {
    let base = &status.base;
    // NOTE: `Threads:\t<n>` is REQUIRED by psutil. `Process.num_threads()`
    // does `int(re.compile(br'Threads:\t(\d+)').findall(data)[0])`, which
    // raises an *uncaught* IndexError (not NoSuchProcess/AccessDenied/
    // NotImplementedError, the only exceptions `Process.as_dict()` swallows)
    // when the line is absent. That crashes any psutil/glances `process_iter`.
    // The tab-separated `Uid:`/`Gid:` lines are likewise mandatory for
    // `Process.uids()`/`gids()`, which also index `findall(...)[0]` blindly.
    format!(
        "Name:\t{}\n\
        State:\t{}\n\
        Tgid:\t{}\n\
        Pid:\t{}\n\
        PPid:\t{}\n\
        TracerPid:\t{}\n\
        Uid:\t{}\t{}\t{}\t{}\n\
        Gid:\t{}\t{}\t{}\t{}\n\
        CapInh:\t{:016x}\n\
        CapPrm:\t{:016x}\n\
        CapEff:\t{:016x}\n\
        CapBnd:\t{:016x}\n\
        CapAmb:\t{:016x}\n\
        Threads:\t{}\n\
        {}\
        Cpus_allowed:\t{}\n\
        Cpus_allowed_list:\t{}\n\
        Mems_allowed:\t1\n\
        Mems_allowed_list:\t0\n\
        voluntary_ctxt_switches:\t0\n\
        nonvoluntary_ctxt_switches:\t0",
        base.name,
        base.state,
        base.tgid,
        base.pid,
        base.ppid,
        base.tracer_pid,
        base.cred.uid, base.cred.euid, base.cred.suid, base.cred.fsuid,
        base.cred.gid, base.cred.egid, base.cred.sgid, base.cred.fsgid,
        base.cred.cap_inheritable,
        base.cred.cap_permitted,
        base.cred.cap_effective,
        base.cred.cap_bounding,
        base.cred.cap_ambient,
        base.num_threads,
        status.mem.format_status_vm_lines(),
        status.cpus_allowed,
        status.cpus_allowed_list,
    )
}

fn format_cpumask_hex(cpumask: AxCpuMask, cpu_num: usize) -> String {
    format_cpu_presence_hex(&collect_cpu_presence(&cpumask, cpu_num))
}

fn format_cpu_presence_hex(cpu_presence: &[bool]) -> String {
    let word_count = cpu_presence.len().div_ceil(32).max(1);
    let mut words = vec![0u32; word_count];

    for (cpu, allowed) in cpu_presence.iter().copied().enumerate() {
        if allowed {
            words[cpu / 32] |= 1u32 << (cpu % 32);
        }
    }

    words
        .iter()
        .rev()
        .map(|word| format!("{word:08x}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn format_cpumask_list(cpumask: AxCpuMask, cpu_num: usize) -> String {
    format_cpu_presence_list(&collect_cpu_presence(&cpumask, cpu_num))
}

fn format_cpu_presence_list(cpu_presence: &[bool]) -> String {
    let mut ranges = Vec::new();
    let mut cpu = 0;

    while cpu < cpu_presence.len() {
        if !cpu_presence[cpu] {
            cpu += 1;
            continue;
        }

        let start = cpu;
        let mut end = cpu;
        while end + 1 < cpu_presence.len() && cpu_presence[end + 1] {
            end += 1;
        }

        ranges.push(if start == end {
            start.to_string()
        } else {
            format!("{start}-{end}")
        });
        cpu = end + 1;
    }

    ranges.join(",")
}

fn collect_cpu_presence<I>(cpus: I, cpu_num: usize) -> Vec<bool>
where
    I: IntoIterator<Item = usize>,
{
    let mut cpu_presence = vec![false; cpu_num];

    for cpu in cpus {
        if cpu < cpu_num {
            cpu_presence[cpu] = true;
        }
    }

    cpu_presence
}

/// The /proc/[pid]/fd directory
struct ThreadFdDir {
    fs: Arc<SimpleFs>,
    task: WeakAxTaskRef,
}

impl SimpleDirOps for ThreadFdDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let Some(task) = self.task.upgrade() else {
            return Box::new(iter::empty());
        };
        let ids = FD_TABLE
            .scope(&task.as_thread().proc_data.scope.read())
            .read()
            .ids()
            .map(|id| Cow::Owned(id.to_string()))
            .collect::<Vec<_>>();
        Box::new(ids.into_iter())
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        let task = self.task.upgrade().ok_or(VfsError::NotFound)?;
        let fd = name.parse::<u32>().map_err(|_| VfsError::NotFound)?;
        let path = FD_TABLE
            .scope(&task.as_thread().proc_data.scope.read())
            .read()
            .get(fd as _)
            .ok_or(VfsError::NotFound)?
            .inner
            .path()
            .into_owned();
        Ok(SimpleFile::new(fs, NodeType::Symlink, move || Ok(path.clone())).into())
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

/// The /proc/[pid]/ns directory — namespace entries.
///
/// Each entry is a regular file displaying the namespace identifier.
/// When opened, the kernel intercepts the open path and creates an
/// [`NsFd`](crate::file::NsFd) instead of a regular file descriptor.
struct NsDir {
    fs: Arc<SimpleFs>,
    task: WeakAxTaskRef,
}

impl SimpleDirOps for NsDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            ["uts", "ipc", "mnt", "pid", "net", "user"]
                .into_iter()
                .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        let task_ref = self.task.clone();
        let Some(task) = task_ref.upgrade() else {
            return Err(VfsError::NotFound);
        };
        let proc_data = &task.as_thread().proc_data;

        let content: String = match name {
            "uts" => {
                let nsproxy = proc_data.nsproxy.lock();
                let nodename = &nsproxy.uts_ns.lock().nodename;
                let nodename_str = core::ffi::CStr::from_bytes_until_nul(unsafe {
                    core::mem::transmute::<&[core::ffi::c_char; 65], &[u8; 65]>(nodename)
                })
                .unwrap_or_default()
                .to_str()
                .unwrap_or_default();
                format!("uts:[{}]\n", nodename_str)
            }
            "ipc" => {
                let nsproxy = proc_data.nsproxy.lock();
                let ns_id = nsproxy.ipc_ns.lock().ns_id;
                format!("ipc:[{}]\n", ns_id)
            }
            "mnt" => "mnt:[root]\n".to_string(),
            "pid" => {
                let nsproxy = proc_data.nsproxy.lock();
                let level = nsproxy.pid_ns.lock().level;
                format!("pid:[{}]\n", level)
            }
            "net" => {
                let nsproxy = proc_data.nsproxy.lock();
                let ns_id = nsproxy.net_ns.lock().ns_id;
                format!("net:[{}]\n", ns_id)
            }
            "user" => {
                let nsproxy = proc_data.nsproxy.lock();
                let inner = nsproxy.user_ns.lock();
                if inner.is_root {
                    "user:[root]\n".to_string()
                } else {
                    format!("user:[{}]\n", inner.owner_uid)
                }
            }
            _ => return Err(VfsError::NotFound),
        };

        let content = content.into_bytes();
        Ok(SimpleFile::new_regular(fs, move || Ok(content.clone())).into())
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

/// The /proc/[pid] directory
struct ThreadDir {
    fs: Arc<SimpleFs>,
    task: WeakAxTaskRef,
    /// Authoritative process state for memory counters (`path_pid` lookup).
    proc_data: Arc<ProcessData>,
    /// Numeric `/proc/<pid>` component used for live [`ProcessData`] lookup.
    path_pid: Pid,
    procfs_pid: Option<Pid>,
}

fn render_thread_maps(task: &WeakAxTaskRef) -> VfsResult<String> {
    let mut output = String::new();

    let task = match task.upgrade() {
        Some(t) => t,
        None => return Ok(output),
    };

    let aspace_arc = task.as_thread().proc_data.aspace();
    let mm = aspace_arc.lock();

    for area in mm.areas() {
        let start = area.start();
        let end = area.end();
        let backend = area.backend();
        let bi = backend.file_info().unwrap_or_else(|_| BackendFileInfo {
            path: String::new(),
            offset: None,
            inode: None,
            dev: None,
            shared: false,
        });
        let BackendFileInfo {
            path,
            offset: file_offset,
            inode,
            dev,
            shared: is_shared,
        } = bi;

        let flag_end = if is_shared { 's' } else { 'p' };
        let flags = area.reported_flags();
        let perms = {
            let r = if flags.contains(MappingFlags::READ) {
                'r'
            } else {
                '-'
            };
            let w = if flags.contains(MappingFlags::WRITE) {
                'w'
            } else {
                '-'
            };
            let x = if flags.contains(MappingFlags::EXECUTE) {
                'x'
            } else {
                '-'
            };
            format!("{}{}{}{}", r, w, x, flag_end)
        };
        const ADDR_HEX_WIDTH: usize = core::mem::size_of::<usize>() * 2;
        const MAPS_COL_WIDTH: usize = 25 + core::mem::size_of::<usize>() * 6 - 1;
        let mut writer = SeqWriter::new(&mut output);

        let dev = dev.map(DeviceId).map(|dev| (dev.major(), dev.minor()));

        write!(
            &mut writer,
            "{:0width$x}-{:0width$x} {} {:08x} {:02x}:{:02x} {}",
            start.as_usize(),
            end.as_usize(),
            perms,
            file_offset.unwrap_or(0),
            dev.map(|(major, _)| major).unwrap_or(0),
            dev.map(|(_, minor)| minor).unwrap_or(0),
            inode.unwrap_or(0),
            width = ADDR_HEX_WIDTH,
        )
        .map_err(|_| VfsError::InvalidInput)?;
        writer.pad_to(MAPS_COL_WIDTH)?;
        if !path.is_empty() {
            writer.write_str(&path)?;
        }
        writer.newline()?;
    }

    Ok(output)
}

/// Render `/proc/[pid]/statm` (process memory in pages).
///
/// Fields (Linux order): `size resident shared text lib data dirty`.
/// psutil's `Process.memory_info()` parses the first 7 ints and computes
/// `memory_percent` from them; the file MUST exist and be parseable, or
/// psutil raises an *uncaught* `FileNotFoundError` (only NoSuchProcess /
/// AccessDenied / ZombieProcess / NotImplementedError are swallowed by
/// `Process.as_dict()`), crashing any `process_iter` (glances / top-likes).
///
/// `size` (VSS) is summed exactly from the mapped areas. `resident` (RSS) comes
/// from incremental address-space counters. `shared` is resident file + shmem
/// pages (Linux `MM_FILEPAGES + MM_SHMEMPAGES`), not VSS or mapcount. `lib`/
/// `dirty` are 0 (Linux also reports 0 for `lib`/`dirty` since 2.6); `text` and
/// `data` are derived from the areas' executable / writable flags.
fn render_thread_statm(
    task: &WeakAxTaskRef,
    proc_data: &Arc<ProcessData>,
    _path_pid: Pid,
) -> VfsResult<String> {
    let _task = match task.upgrade() {
        Some(t) => t,
        None => return Ok("0 0 0 0 0 0 0\n".into()),
    };
    let aspace_arc = proc_data.aspace();
    let mm = aspace_arc.lock();
    Ok(ProcessMemStats::collect(&mm).format_statm())
}

fn render_thread_stat(
    task: &WeakAxTaskRef,
    proc_data: &Arc<ProcessData>,
    _path_pid: Pid,
    procfs_pid: Option<Pid>,
) -> VfsResult<Vec<u8>> {
    let task = task.upgrade().ok_or(VfsError::NotFound)?;
    let mut stat = TaskStat::from_thread(&task)?;
    let aspace_arc = proc_data.aspace();
    let mem = ProcessMemStats::collect(&aspace_arc.lock());
    stat.vsize = mem.vsize_bytes();
    stat.rss = mem.rss_pages();
    stat.start_code = mem.start_code;
    stat.end_code = mem.end_code;
    stat.start_stack = mem.start_stack;
    stat.start_brk = proc_data.get_heap_top() as u64;
    if let Some(pid) = procfs_pid {
        stat.pid = pid;
    }
    Ok(format!("{stat}").into_bytes())
}

fn render_thread_auxv(task: &AxTaskRef) -> Vec<u8> {
    let mut entries = task.as_thread().proc_data.auxv.read().clone();
    entries.push(AuxEntry::new(AuxType::NULL, 0));
    let mut bytes = Vec::with_capacity(entries.len() * size_of::<AuxEntry>());
    for entry in entries {
        bytes.extend_from_slice(entry.as_bytes());
    }
    bytes
}

struct ProcMemFile {
    proc_data: Arc<ProcessData>,
}

impl ProcMemFile {
    fn check_access(&self) -> VfsResult<()> {
        let current_task = current();
        let current_proc = &current_task.as_thread().proc_data;
        if current_proc.proc.pid() == self.proc_data.proc.pid() {
            return Ok(());
        }

        let is_tracer = (self.proc_data.is_ptrace_traceme() || self.proc_data.is_ptrace_attached())
            && self
                .proc_data
                .ptrace_tracer_pid()
                .is_some_and(|pid| pid == current_proc.proc.pid())
            && self.proc_data.ptrace_stop_signo().is_some();
        if is_tracer {
            Ok(())
        } else {
            Err(VfsError::PermissionDenied)
        }
    }

    fn populate_remote_range(&self, addr: usize, len: usize, flags: MappingFlags) -> VfsResult<()> {
        if len == 0 {
            return Ok(());
        }
        let start = VirtAddr::from_usize(addr);
        let end = VirtAddr::from_usize(addr.checked_add(len).ok_or(VfsError::BadAddress)?);
        let page_start = start.align_down_4k();
        let page_end = end.align_up_4k();
        let aspace = self.proc_data.aspace();
        let mut aspace = aspace.lock();
        aspace.populate_area(page_start, page_end - page_start, flags)
    }
}

impl DirectRwFsFileOps for ProcMemFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        self.check_access()?;
        if buf.is_empty() {
            return Ok(0);
        }
        let addr = usize::try_from(offset).map_err(|_| VfsError::BadAddress)?;
        self.populate_remote_range(addr, buf.len(), MappingFlags::READ)?;
        let aspace = self.proc_data.aspace();
        let aspace = aspace.lock();
        aspace.read(VirtAddr::from_usize(addr), buf)?;
        Ok(buf.len())
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        self.check_access()?;
        if buf.is_empty() {
            return Ok(0);
        }
        let addr = usize::try_from(offset).map_err(|_| VfsError::BadAddress)?;
        self.populate_remote_range(addr, buf.len(), MappingFlags::WRITE)?;
        let aspace = self.proc_data.aspace();
        let aspace = aspace.lock();
        aspace.write(VirtAddr::from_usize(addr), buf)?;
        ax_runtime::hal::cache::flush_icache_all();
        Ok(buf.len())
    }
}

impl SimpleDirOps for ThreadDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            [
                "stat",
                "statm",
                "status",
                "oom_score_adj",
                "task",
                "maps",
                "mem",
                "auxv",
                "mounts",
                "cmdline",
                "comm",
                "exe",
                "fd",
                "uid_map",
                "gid_map",
                "setgroups",
                "cgroup",
                "ns",
            ]
            .into_iter()
            .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        let task = self.task.upgrade().ok_or(VfsError::NotFound)?;
        Ok(match name {
            "stat" => {
                let task = self.task.clone();
                let proc_data = self.proc_data.clone();
                let path_pid = self.path_pid;
                let procfs_pid = self.procfs_pid;
                SimpleFile::new_regular(fs, move || {
                    render_thread_stat(&task, &proc_data, path_pid, procfs_pid)
                })
                .into()
            }
            "statm" => {
                let task = self.task.clone();
                let proc_data = self.proc_data.clone();
                let path_pid = self.path_pid;
                SimpleFile::new_regular(fs, move || {
                    render_thread_statm(&task, &proc_data, path_pid)
                })
                .into()
            }
            "status" => {
                let task = self.task.clone();
                let proc_data = self.proc_data.clone();
                let path_pid = self.path_pid;
                let procfs_pid = self.procfs_pid;
                SimpleFile::new_regular(fs, move || {
                    render_thread_status(&task, &proc_data, path_pid, procfs_pid)
                })
                .into()
            }
            "oom_score_adj" => SimpleFile::new_regular(
                fs,
                RwFile::new(move |req| match req {
                    SimpleFileOperation::Read => Ok(Some(
                        task.as_thread().oom_score_adj().to_string().into_bytes(),
                    )),
                    SimpleFileOperation::Write(data) => {
                        if !data.is_empty() {
                            let value = str::from_utf8(data)
                                .ok()
                                .and_then(|it| it.parse::<i32>().ok())
                                .ok_or(VfsError::InvalidInput)?;
                            task.as_thread().set_oom_score_adj(value);
                        }
                        Ok(None)
                    }
                }),
            )
            .into(),
            "task" => SimpleDir::new_maker(
                fs.clone(),
                Arc::new(ProcessTaskDir {
                    fs,
                    process: Arc::downgrade(&task.as_thread().proc_data.proc),
                }),
            )
            .into(),
            "maps" => {
                let task = self.task.clone();
                let seq = SeqObject::new(move || render_thread_maps(&task));
                SpecialFsFile::new_regular_with_perm(
                    fs.clone(),
                    seq,
                    NodePermission::from_bits_truncate(0o444),
                )
                .into()
            }
            "mem" => SpecialFsFile::new_regular_with_perm(
                fs,
                ProcMemFile {
                    proc_data: task.as_thread().proc_data.clone(),
                },
                NodePermission::from_bits_truncate(0o600),
            )
            .into(),
            "auxv" => SimpleFile::new_regular(fs, move || Ok(render_thread_auxv(&task))).into(),
            "mounts" => SimpleFile::new_regular(fs, move || {
                Ok("proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n")
            })
            .into(),
            "cmdline" => SimpleFile::new_regular(fs, move || {
                let cmdline = task.as_thread().proc_data.cmdline.read();
                let mut buf = Vec::new();
                for arg in cmdline.iter() {
                    buf.extend_from_slice(arg.as_bytes());
                    buf.push(0);
                }
                Ok(buf)
            })
            .into(),
            "comm" => SimpleFile::new_regular(
                fs,
                RwFile::new(move |req| match req {
                    SimpleFileOperation::Read => {
                        let mut bytes = vec![0; 16];
                        let name = task.name();
                        let copy_len = name.len().min(15);
                        bytes[..copy_len].copy_from_slice(&name.as_bytes()[..copy_len]);
                        bytes[copy_len] = b'\n';
                        Ok(Some(bytes))
                    }
                    SimpleFileOperation::Write(data) => {
                        if !data.is_empty() {
                            let mut input = [0; 16];
                            let copy_len = data.len().min(15);
                            input[..copy_len].copy_from_slice(&data[..copy_len]);
                            task.set_name(
                                CStr::from_bytes_until_nul(&input)
                                    .map_err(|_| VfsError::InvalidInput)?
                                    .to_str()
                                    .map_err(|_| VfsError::InvalidInput)?,
                            );
                        }
                        Ok(None)
                    }
                }),
            )
            .into(),
            "exe" => SimpleFile::new(fs, NodeType::Symlink, move || {
                Ok(task.as_thread().proc_data.exe_path.read().clone())
            })
            .into(),
            "fd" => SimpleDir::new_maker(
                fs.clone(),
                Arc::new(ThreadFdDir {
                    fs,
                    task: Arc::downgrade(&task),
                }),
            )
            .into(),
            "uid_map" => SimpleFile::new_regular(
                fs,
                RwFile::new(move |req| match req {
                    SimpleFileOperation::Read => {
                        let thr = task.as_thread();
                        let cred = thr.cred();
                        let content = if thr.uid_map_written() || cred.euid != 65534 {
                            format!("         0  {:>10} 4294967295\n", cred.uid)
                        } else {
                            "\n".to_string()
                        };
                        Ok(Some(content.into_bytes()))
                    }
                    SimpleFileOperation::Write(data) => {
                        let input =
                            core::str::from_utf8(data).map_err(|_| VfsError::InvalidInput)?;
                        // Linux uid_map format: <lower_uid> <upper_uid> <count>
                        // Maps UIDs in the parent namespace (lower_uid..lower_uid+count)
                        // to UIDs in this namespace (upper_uid..upper_uid+count).
                        //
                        // StarryOS simplified semantics: we do not maintain namespace
                        // UID mappings; instead we directly set the thread's credentials
                        // to the upper_uid value (the UID this namespace wants to see).
                        // For the common `0 0 1` case (map root to root) this is correct.
                        // For non-trivial mappings this is an intentional simplification
                        // — StarryOS does not implement full user namespacing.
                        let parts: Vec<&str> = input.split_whitespace().collect();
                        if parts.len() >= 3 {
                            let _mapped: u32 =
                                parts[0].parse().map_err(|_| VfsError::InvalidInput)?;
                            let orig: u32 = parts[1].parse().map_err(|_| VfsError::InvalidInput)?;
                            let _count: u32 =
                                parts[2].parse().map_err(|_| VfsError::InvalidInput)?;
                            let thr = task.as_thread();
                            let mut cred = (*thr.cred()).clone();
                            cred.uid = orig;
                            cred.euid = orig;
                            cred.suid = orig;
                            cred.fsuid = orig;
                            if orig == 0 {
                                let mask = Cred::cap_mask();
                                cred.cap_permitted = mask;
                                cred.cap_effective = mask;
                                cred.cap_bounding = mask;
                            } else {
                                cred.cap_permitted = 0;
                                cred.cap_effective = 0;
                                cred.cap_ambient = 0;
                            }
                            cred.sanitize_capabilities();
                            Thread::set_cred(thr, cred);
                            thr.set_uid_map_written(true);
                            // Mark the user namespace as UID-mapped so
                            // getuid/geteuid/getresuid return the mapped
                            // value instead of 65534 (nobody).
                            let proc_data = &thr.proc_data;
                            let nsproxy = proc_data.nsproxy.lock();
                            nsproxy.user_ns.lock().uid_mapped = true;
                        }
                        Ok(None)
                    }
                }),
            )
            .into(),
            "gid_map" => SimpleFile::new_regular(
                fs,
                RwFile::new(move |req| match req {
                    SimpleFileOperation::Read => {
                        let thr = task.as_thread();
                        let cred = thr.cred();
                        let content = if thr.gid_map_written() || cred.egid != 65534 {
                            format!("         0  {:>10} 4294967295\n", cred.gid)
                        } else {
                            "\n".to_string()
                        };
                        Ok(Some(content.into_bytes()))
                    }
                    SimpleFileOperation::Write(data) => {
                        let input =
                            core::str::from_utf8(data).map_err(|_| VfsError::InvalidInput)?;
                        // Linux gid_map format: <lower_gid> <upper_gid> <count>
                        // Same simplified semantics as uid_map above.
                        //
                        // StarryOS does not maintain namespace GID mappings;
                        // it directly sets the thread's credentials to upper_gid.
                        let parts: Vec<&str> = input.split_whitespace().collect();
                        if parts.len() >= 3 {
                            let _mapped: u32 =
                                parts[0].parse().map_err(|_| VfsError::InvalidInput)?;
                            let orig: u32 = parts[1].parse().map_err(|_| VfsError::InvalidInput)?;
                            let _count: u32 =
                                parts[2].parse().map_err(|_| VfsError::InvalidInput)?;
                            let thr = task.as_thread();
                            let mut cred = (*thr.cred()).clone();
                            cred.gid = orig;
                            cred.egid = orig;
                            cred.sgid = orig;
                            cred.fsgid = orig;
                            cred.sanitize_capabilities();
                            Thread::set_cred(thr, cred);
                            thr.set_gid_map_written(true);
                            let proc_data = &thr.proc_data;
                            let nsproxy = proc_data.nsproxy.lock();
                            nsproxy.user_ns.lock().gid_mapped = true;
                        }
                        Ok(None)
                    }
                }),
            )
            .into(),
            "setgroups" => SimpleFile::new_regular(
                fs,
                RwFile::new(move |req| match req {
                    SimpleFileOperation::Read => {
                        let thr = task.as_thread();
                        let content = if thr.setgroups_deny() {
                            "deny\n"
                        } else {
                            "allow\n"
                        };
                        Ok(Some(content.as_bytes().to_vec()))
                    }
                    SimpleFileOperation::Write(data) => {
                        let input = core::str::from_utf8(data)
                            .map_err(|_| VfsError::InvalidInput)?
                            .trim();
                        if input == "deny" {
                            task.as_thread().set_setgroups_deny(true);
                        } else if input == "allow" {
                            task.as_thread().set_setgroups_deny(false);
                        }
                        Ok(None)
                    }
                }),
            )
            .into(),
            "cgroup" => SimpleFile::new_regular(fs, move || Ok("0::/\n")).into(),
            "ns" => SimpleDir::new_maker(
                fs.clone(),
                Arc::new(NsDir {
                    fs,
                    task: self.task.clone(),
                }),
            )
            .into(),
            _ => return Err(VfsError::NotFound),
        })
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

/// Handles /proc/[pid] & /proc/self
struct ProcFsHandler(Arc<SimpleFs>);

impl SimpleDirOps for ProcFsHandler {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            processes()
                .into_iter()
                .map(|proc_data| procfs_visible_pid(&proc_data.proc).to_string().into())
                .chain([Cow::Borrowed("self")]),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let (task, path_pid, procfs_pid, proc_data) = if name == "self" {
            let task = current().clone();
            let path_pid = task.as_thread().proc_data.proc.pid();
            let proc_data = procfs_lookup_process(path_pid).map_err(|_| VfsError::NotFound)?;
            (task, path_pid, None, proc_data)
        } else {
            let pid = name.parse::<u32>().map_err(|_| VfsError::NotFound)?;
            let proc_data = procfs_lookup_process(pid).map_err(|_| VfsError::NotFound)?;
            let task = if let Ok(task) = get_task(pid) {
                task
            } else {
                let tid = proc_data
                    .proc
                    .threads()
                    .into_iter()
                    .next()
                    .ok_or(VfsError::NotFound)?;
                get_task(tid).map_err(|_| VfsError::NotFound)?
            };
            let procfs_pid =
                (procfs_visible_pid(&proc_data.proc) != proc_data.proc.pid()).then_some(pid);
            (task, pid, procfs_pid, proc_data)
        };
        let node = NodeOpsMux::Dir(SimpleDir::new_maker(
            self.0.clone(),
            Arc::new(ThreadDir {
                fs: self.0.clone(),
                task: Arc::downgrade(&task),
                proc_data,
                path_pid,
                procfs_pid,
            }),
        ));
        Ok(node)
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

fn builder(fs: Arc<SimpleFs>) -> DirMaker {
    let mut root = DirMapping::new();
    root.add(
        "mounts",
        SimpleFile::new_regular(fs.clone(), || {
            Ok("proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n")
        }),
    );
    // /proc/filesystems — list of registered filesystem types. Tools like
    // `mount`/`findmnt` and some container runtimes read it to decide what they
    // can mount; absence (ENOENT) made those probes fail.
    root.add(
        "filesystems",
        SimpleFile::new_regular(fs.clone(), || {
            Ok("nodev\tsysfs\nnodev\tproc\nnodev\ttmpfs\nnodev\tdevtmpfs\n\text4\n")
        }),
    );
    root.add(
        "stat",
        SimpleFile::new_regular(fs.clone(), || Ok(render_stat())),
    );
    root.add(
        "meminfo",
        SimpleFile::new_regular(fs.clone(), || Ok(render_meminfo())),
    );
    root.add(
        "cpuinfo",
        SimpleFile::new_regular(fs.clone(), || Ok(render_cpuinfo())),
    );
    root.add(
        "uptime",
        SimpleFile::new_regular(fs.clone(), || {
            let up = monotonic_time();
            let secs = up.as_secs();
            let cs = up.subsec_millis() / 10;
            // Approximate total idle as uptime × cpu_count (no per-CPU idle accounting yet).
            let idle_secs = secs.saturating_mul(ax_runtime::hal::cpu_num() as u64);
            Ok(format!("{secs}.{cs:02} {idle_secs}.00\n"))
        }),
    );
    root.add(
        "loadavg",
        SimpleFile::new_regular(fs.clone(), || {
            let all_tasks = tasks();
            let running = all_tasks
                .iter()
                .filter(|t| matches!(t.state(), TaskState::Running | TaskState::Ready))
                .count();
            let total = all_tasks.len();
            Ok(format!("0.00 0.00 0.00 {running}/{total} 1\n"))
        }),
    );
    root.add(
        "meminfo2",
        SimpleFile::new_regular(fs.clone(), || {
            let allocator = ax_alloc::global_allocator();
            Ok(format!("{:?}\n", allocator.usages()))
        }),
    );
    root.add(
        "instret",
        SimpleFile::new_regular(fs.clone(), || {
            #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
            {
                Ok(format!("{}\n", riscv::register::instret::read64()))
            }
            #[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
            {
                Ok("0\n".to_string())
            }
        }),
    );
    // Timer-tick callbacks registered once on the boot CPU.
    // IRQ counting: increment the module-level IRQ_CNT on every tick.
    ax_task::register_timer_callback(|_| {
        IRQ_CNT.fetch_add(1, Ordering::Relaxed);
    });
    // CPU-time accounting: accumulate utime/stime for the running task on
    // each tick, so preempted tasks don't have to wait until the next syscall
    // to record their CPU usage.
    // Note: this callback runs only on the boot CPU (TIMER_CALLBACKS is
    // per-CPU).  On SMP, tasks on other CPUs still get their time recorded
    // at syscall boundaries via set_timer_state(); the tick path is an
    // additional precision improvement for CPU 0.
    ax_task::register_timer_callback(|_| {
        tick_cpu_time(&ax_task::current());
    });

    root.add(
        "interrupts",
        SimpleFile::new_regular(fs.clone(), || {
            Ok(format!("0: {}", IRQ_CNT.load(Ordering::Relaxed)))
        }),
    );

    root.add("sys", {
        let mut sys = DirMapping::new();

        sys.add("kernel", {
            let mut kernel = DirMapping::new();

            kernel.add(
                "pid_max",
                SimpleFile::new_regular(fs.clone(), || Ok("32768\n")),
            );
            kernel.add(
                "osrelease",
                SimpleFile::new_regular(fs.clone(), || Ok("6.6.0-starry\n")),
            );
            kernel.add(
                "ostype",
                SimpleFile::new_regular(fs.clone(), || Ok("Linux\n")),
            );

            // perf knobs the upstream Linux `perf` tool probes at startup.
            // `perf_event_paranoid` gates how much unprivileged users may
            // measure; -1 is the most permissive setting (kernel/CPU/tracepoint
            // events all allowed) so perf can profile freely here.
            kernel.add(
                "perf_event_paranoid",
                SimpleFile::new_regular(fs.clone(), || Ok("-1\n")),
            );
            // Per-user locked pages for the perf ring buffer; Linux default.
            kernel.add(
                "perf_event_mlock_kb",
                SimpleFile::new_regular(fs.clone(), || Ok("516\n")),
            );
            // Upper bound perf uses to clamp the requested sample frequency (-F).
            kernel.add(
                "perf_event_max_sample_rate",
                SimpleFile::new_regular(fs.clone(), || Ok("100000\n")),
            );

            SimpleDir::new_maker(fs.clone(), Arc::new(kernel))
        });

        // /proc/sys/vm — read-only constants several runtimes probe at startup.
        // `max_map_count` in particular is read by Elasticsearch/Lucene and some
        // JVMs as a preflight check; its absence (ENOENT) trips those checks.
        sys.add("vm", {
            let mut vm = DirMapping::new();
            vm.add(
                "overcommit_memory",
                SimpleFile::new_regular(fs.clone(), || Ok("0\n")),
            );
            vm.add(
                "max_map_count",
                SimpleFile::new_regular(fs.clone(), || Ok("65530\n")),
            );
            SimpleDir::new_maker(fs.clone(), Arc::new(vm))
        });

        // /proc/sys/fs — file-descriptor limits some servers read to size tables.
        sys.add("fs", {
            let mut fs_sys = DirMapping::new();
            fs_sys.add(
                "file-max",
                SimpleFile::new_regular(fs.clone(), || Ok("1048576\n")),
            );
            fs_sys.add(
                "nr_open",
                SimpleFile::new_regular(fs.clone(), || Ok("1048576\n")),
            );
            SimpleDir::new_maker(fs.clone(), Arc::new(fs_sys))
        });

        // /proc/sys/net/core/somaxconn — listen-backlog clamp some servers read.
        sys.add("net", {
            let mut net = DirMapping::new();
            net.add("core", {
                let mut core = DirMapping::new();
                core.add(
                    "somaxconn",
                    SimpleFile::new_regular(fs.clone(), || Ok("4096\n")),
                );
                SimpleDir::new_maker(fs.clone(), Arc::new(core))
            });
            SimpleDir::new_maker(fs.clone(), Arc::new(net))
        });

        SimpleDir::new_maker(fs.clone(), Arc::new(sys))
    });

    root.add("net", {
        let mut net = DirMapping::new();

        net.add(
            "arp",
            SimpleFile::new_regular(fs.clone(), || Ok(render_proc_net_arp())),
        );
        net.add(
            "dev",
            SimpleFile::new_regular(fs.clone(), || Ok(render_proc_net_dev())),
        );

        SimpleDir::new_maker(fs.clone(), Arc::new(net))
    });

    // /proc/device-tree/{compatible,model} — minimal Open Firmware view from the
    // live FDT, so SoC-detecting userspace (e.g. librockchip_mpp's read_soc_name)
    // can identify the chip. Built only for the JPU/MPP path (`jpeg` feature) and
    // only when a real FDT actually provides the values; the raw bytes are exposed
    // verbatim and never fabricated on non-FDT platforms. A full FDT mirror is
    // unnecessary (only MPP reads device-tree; librga/rknn use their own nodes).
    #[cfg(feature = "jpeg")]
    if let Some(compatible) = read_dt_root_property("compatible") {
        root.add("device-tree", {
            let mut dt = DirMapping::new();
            dt.add(
                "compatible",
                SimpleFile::new_regular(fs.clone(), move || Ok(compatible.clone())),
            );
            if let Some(model) = read_dt_root_property("model") {
                dt.add(
                    "model",
                    SimpleFile::new_regular(fs.clone(), move || Ok(model.clone())),
                );
            }
            SimpleDir::new_maker(fs.clone(), Arc::new(dt))
        });
    }

    root.add("dynamic_debug", {
        let mut dynamic_debug = DirMapping::new();

        dynamic_debug.add(
            "control",
            super::dyn_debug::create_dyn_debug_control_file(fs.clone()),
        );

        SimpleDir::new_maker(fs.clone(), Arc::new(dynamic_debug))
    });

    static ALL_SYMS: LazyInit<String> = LazyInit::new();

    let ksym = read_kallsyms();
    KALLSYMS.init_once(ksym);

    root.add("kallsyms", {
        if !ALL_SYMS.is_inited() {
            ALL_SYMS.init_once(KALLSYMS.dump_all_symbols());
        }
        let seq_obj = SeqObject::new(|| Ok(ALL_SYMS.as_str()));
        SpecialFsFile::new_regular_with_perm(
            fs.clone(),
            seq_obj,
            NodePermission::from_bits_truncate(0o444),
        )
    });

    let proc_dir = ProcFsHandler(fs.clone());
    SimpleDir::new_maker(fs, Arc::new(proc_dir.chain(root)))
}

pub struct SeqWriter<W: core::fmt::Write> {
    inner: W,
    col: usize,
}

impl<W: core::fmt::Write> SeqWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner, col: 0 }
    }
}

impl<W: core::fmt::Write> SeqWriter<W> {
    fn write_str(&mut self, s: &str) -> VfsResult<()> {
        self.col += s.len();
        self.inner.write_str(s)?;
        Ok(())
    }

    #[allow(unused)]
    fn write_char(&mut self, c: char) -> VfsResult<()> {
        self.col += c.len_utf8();
        self.inner.write_char(c)?;
        Ok(())
    }

    fn pad_to(&mut self, target: usize) -> VfsResult<()> {
        if self.col < target {
            let pad = target - self.col;
            for _ in 0..pad {
                self.inner.write_char(' ')?;
            }
            self.col = target;
        }
        Ok(())
    }

    fn newline(&mut self) -> VfsResult<()> {
        self.inner.write_char('\n')?;
        self.col = 0;
        Ok(())
    }
}

impl<W: core::fmt::Write> core::fmt::Write for SeqWriter<W> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_str(s).map_err(|_| core::fmt::Error)
    }
}
#[cfg(test)]
mod tests {
    use alloc::{format, string::String};

    use super::{
        TaskStatusBase, TaskStatusFields, collect_cpu_presence, format_cpu_presence_hex,
        format_cpu_presence_list, render_task_status_fields,
    };
    use crate::{mm::ProcessMemStats, task::Cred};

    fn sample_mem_stats() -> ProcessMemStats {
        ProcessMemStats {
            vss_pages: 128,
            resident_pages: 128,
            ..Default::default()
        }
    }

    fn legacy_render_task_status(tgid: u32, pid: u64) -> String {
        format!(
            "Tgid:\t{}\nPid:\t{}\nUid:\t0 0 0 0\nGid:\t0 0 0 \
             0\nCpus_allowed:\t1\nCpus_allowed_list:\t0\nMems_allowed:\t1\nMems_allowed_list:\t0",
            tgid, pid
        )
    }

    fn render_task_status_from_cpus(tgid: u32, pid: u64, cpus: &[usize], cpu_num: usize) -> String {
        let cpu_presence = collect_cpu_presence(cpus.iter().copied(), cpu_num);
        let cpus_allowed = format_cpu_presence_hex(&cpu_presence);
        let cpus_allowed_list = format_cpu_presence_list(&cpu_presence);

        render_task_status_fields(&TaskStatusFields {
            base: TaskStatusBase {
                name: "proc-status-test",
                state: "R (running)",
                tgid,
                pid,
                ppid: 0,
                tracer_pid: 0,
                cred: &Cred::root(),
                num_threads: 1,
            },
            cpus_allowed: &cpus_allowed,
            cpus_allowed_list: &cpus_allowed_list,
            mem: &sample_mem_stats(),
        })
    }

    #[test]
    fn old_hardcoded_status_lies_about_non_cpu0_affinity() {
        let legacy = legacy_render_task_status(42, 84);

        assert!(legacy.contains("Cpus_allowed:\t1\n"));
        assert!(legacy.contains("Cpus_allowed_list:\t0\n"));
        assert!(!legacy.contains("Cpus_allowed:\t0000000a\n"));
        assert!(!legacy.contains("Cpus_allowed_list:\t1,3\n"));
    }

    #[test]
    fn cpus_allowed_hex_matches_actual_affinity_bits() {
        let cpu_presence = collect_cpu_presence([1, 3], 4);

        assert_eq!(format_cpu_presence_hex(&cpu_presence), "0000000a");
    }

    #[test]
    fn cpus_allowed_hex_orders_32bit_words_from_high_to_low() {
        let cpu_presence = collect_cpu_presence([0, 1, 32, 63], 64);

        assert_eq!(format_cpu_presence_hex(&cpu_presence), "80000001,00000003");
    }

    #[test]
    fn cpus_allowed_list_compacts_contiguous_ranges() {
        let cpu_presence = collect_cpu_presence([0, 2, 3, 4, 7, 9, 10, 11], 12);

        assert_eq!(format_cpu_presence_list(&cpu_presence), "0,2-4,7,9-11");
    }

    #[test]
    fn task_status_reports_real_affinity_instead_of_cpu0_only() {
        let status = render_task_status_from_cpus(42, 84, &[1, 3], 4);

        assert!(status.contains("Tgid:\t42\n"));
        assert!(status.contains("Pid:\t84\n"));
        assert!(status.contains("Name:\tproc-status-test\n"));
        assert!(status.contains("State:\tR (running)\n"));
        assert!(status.contains("PPid:\t0\n"));
        assert!(status.contains("Cpus_allowed:\t0000000a\n"));
        assert!(status.contains("Cpus_allowed_list:\t1,3\n"));
    }

    #[test]
    fn task_status_emits_tab_separated_threads_line_for_psutil() {
        // psutil `Process.num_threads()` parses this line with the regex
        // `br'Threads:\t(\d+)'` and blindly indexes `[0]`; a missing line
        // raises an uncaught IndexError that crashes glances' process_iter.
        let cpu_presence = collect_cpu_presence([0usize], 1);
        let cpus_allowed = format_cpu_presence_hex(&cpu_presence);
        let cpus_allowed_list = format_cpu_presence_list(&cpu_presence);
        let status = render_task_status_fields(&TaskStatusFields {
            base: TaskStatusBase {
                name: "proc-status-test",
                state: "S (sleeping)",
                tgid: 1,
                pid: 1,
                ppid: 0,
                tracer_pid: 0,
                cred: &Cred::root(),
                num_threads: 3,
            },
            cpus_allowed: &cpus_allowed,
            cpus_allowed_list: &cpus_allowed_list,
            mem: &sample_mem_stats(),
        });

        assert!(status.contains("Threads:\t3\n"));
        // Tab-separated, exactly as the psutil regex expects (not space).
        assert!(!status.contains("Threads: 3"));
        assert!(status.contains("State:\tS (sleeping)\n"));
    }

    #[test]
    fn task_status_reports_tracer_pid_for_debuggers() {
        let cpu_presence = collect_cpu_presence([0usize], 1);
        let cpus_allowed = format_cpu_presence_hex(&cpu_presence);
        let cpus_allowed_list = format_cpu_presence_list(&cpu_presence);
        let status = render_task_status_fields(&TaskStatusFields {
            base: TaskStatusBase {
                name: "proc-status-test",
                state: "R (running)",
                tgid: 10,
                pid: 11,
                ppid: 9,
                tracer_pid: 42,
                cred: &Cred::root(),
                num_threads: 1,
            },
            cpus_allowed: &cpus_allowed,
            cpus_allowed_list: &cpus_allowed_list,
            mem: &sample_mem_stats(),
        });

        assert!(status.contains("PPid:\t9\n"));
        assert!(status.contains("TracerPid:\t42\n"));
    }

    #[test]
    fn status_includes_vm_size_from_mem_stats() {
        let cpu_presence = collect_cpu_presence([0usize], 1);
        let cpus_allowed = format_cpu_presence_hex(&cpu_presence);
        let cpus_allowed_list = format_cpu_presence_list(&cpu_presence);
        let mem = ProcessMemStats {
            vss_pages: 128,
            resident_pages: 128,
            ..Default::default()
        };
        let status = render_task_status_fields(&TaskStatusFields {
            base: TaskStatusBase {
                name: "proc-status-test",
                state: "R (running)",
                tgid: 1,
                pid: 1,
                ppid: 0,
                tracer_pid: 0,
                cred: &Cred::root(),
                num_threads: 1,
            },
            cpus_allowed: &cpus_allowed,
            cpus_allowed_list: &cpus_allowed_list,
            mem: &mem,
        });

        assert!(status.contains("VmSize:\t512 kB\n"));
        assert!(status.contains("VmRSS:\t512 kB\n"));
    }
}
