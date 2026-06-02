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
use ax_memory_addr::PAGE_SIZE_4K;
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
    mm::BackendFileInfo,
    pseudofs::{
        DirMaker, DirMapping, NodeOpsMux, RwFile, SeqObject, SimpleDir, SimpleDirOps, SimpleFile,
        SimpleFileOperation, SimpleFs, SpecialFsFile,
    },
    task::{
        AsThread, ProcessData, TaskStat, Thread, get_process_data, get_task, processes, tasks,
        tick_cpu_time,
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
    let free = total.saturating_sub(used);

    let total_kb = total / 1024;
    let free_kb = free / 1024;
    let cached_kb = cached / 1024;
    let available_kb = free_kb + cached_kb;

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
         AnonPages:               0 kB\n\
         Mapped:                  0 kB\n\
         Shmem:                   0 kB\n\
         KReclaimable:            0 kB\n\
         Slab:                    0 kB\n\
         SReclaimable:            0 kB\n\
         SUnreclaim:              0 kB\n\
         KernelStack:             0 kB\n\
         PageTables:              0 kB\n\
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
    let _ = writeln!(buf, "processor\t: {idx}");
    let _ = writeln!(buf, "BogoMIPS\t: 100.00");
    let _ = writeln!(buf, "CPU implementer\t: 0x00");
    let _ = writeln!(buf, "CPU architecture: 8");
    let _ = writeln!(buf, "CPU variant\t: 0x0");
    let _ = writeln!(buf, "CPU part\t: 0x000");
    let _ = writeln!(buf, "CPU revision\t: 0");
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
    let mut entries = axnet::arp_entries();
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
    "Inter-|   Receive                                                |  Transmit\n\
      face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n\
        lo:       0       0    0    0    0     0          0         0        0       0    0    0    0     0       0          0\n\
      eth0:       0       0    0    0    0     0          0         0        0       0    0    0    0     0       0          0\n"
        .to_string()
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

        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            self.fs.clone(),
            Arc::new(ThreadDir {
                fs: self.fs.clone(),
                task: Arc::downgrade(&task),
                procfs_pid: None,
            }),
        )))
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

fn task_status(task: &AxTaskRef) -> String {
    let thread = task.as_thread();
    let cred = thread.cred();
    let num_threads = thread.proc_data.proc.threads().len() as u32;
    render_task_status(
        thread.proc_data.proc.pid(),
        thread.tid() as u64,
        &cred,
        num_threads,
        task.cpumask(),
        ax_runtime::hal::cpu_num(),
    )
}

fn render_task_status(
    tgid: u32,
    pid: u64,
    cred: &crate::task::Cred,
    num_threads: u32,
    cpumask: AxCpuMask,
    cpu_num: usize,
) -> String {
    let cpus_allowed = format_cpumask_hex(cpumask, cpu_num);
    let cpus_allowed_list = format_cpumask_list(cpumask, cpu_num);

    render_task_status_fields(
        tgid,
        pid,
        cred,
        num_threads,
        &cpus_allowed,
        &cpus_allowed_list,
    )
}

#[rustfmt::skip]
fn render_task_status_fields(
    tgid: u32,
    pid: u64,
    cred: &crate::task::Cred,
    num_threads: u32,
    cpus_allowed: &str,
    cpus_allowed_list: &str,
) -> String {
    // NOTE: `Threads:\t<n>` is REQUIRED by psutil. `Process.num_threads()`
    // does `int(re.compile(br'Threads:\t(\d+)').findall(data)[0])`, which
    // raises an *uncaught* IndexError (not NoSuchProcess/AccessDenied/
    // NotImplementedError, the only exceptions `Process.as_dict()` swallows)
    // when the line is absent. That crashes any psutil/glances `process_iter`.
    // The tab-separated `Uid:`/`Gid:` lines are likewise mandatory for
    // `Process.uids()`/`gids()`, which also index `findall(...)[0]` blindly.
    format!(
        "Tgid:\t{tgid}\n\
        Pid:\t{pid}\n\
        Uid:\t{}\t{}\t{}\t{}\n\
        Gid:\t{}\t{}\t{}\t{}\n\
        Threads:\t{num_threads}\n\
        Cpus_allowed:\t{cpus_allowed}\n\
        Cpus_allowed_list:\t{cpus_allowed_list}\n\
        Mems_allowed:\t1\n\
        Mems_allowed_list:\t0",
        cred.uid, cred.euid, cred.suid, cred.fsuid,
        cred.gid, cred.egid, cred.sgid, cred.fsgid,
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
        let BackendFileInfo {
            path,
            offset: file_offset,
            inode,
            dev,
            shared: is_shared,
        } = backend.file_info()?;

        let flag_end = if is_shared { 's' } else { 'p' };
        let flags = area.flags();
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
        const MAPS_COL_WIDTH: usize = 25 + core::mem::size_of::<usize>() * 6 - 1;
        let mut writer = SeqWriter::new(&mut output);

        let dev = dev.map(DeviceId).map(|dev| (dev.major(), dev.minor()));

        write!(
            &mut writer,
            "{:08x}-{:08x} {} {:08x} {:02x}:{:02x} {}",
            start.as_usize(),
            end.as_usize(),
            perms,
            file_offset.unwrap_or(0),
            dev.map(|(major, _)| major).unwrap_or(0),
            dev.map(|(_, minor)| minor).unwrap_or(0),
            inode.unwrap_or(0),
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
/// `size` (VSS) is summed exactly from the mapped areas. `resident` (RSS) is
/// reported as the full VSS — an honest upper bound rather than a fabricated
/// figure; StarryOS' file-backed areas are lazily/cache populated, so a precise
/// per-page page-table walk for every process on every refresh would be far too
/// expensive under emulation. `shared`/`lib`/`dirty` are 0 (Linux also reports 0
/// for `lib`/`dirty` since 2.6); `text` and `data` are derived from the areas'
/// executable / writable flags.
fn render_thread_statm(task: &WeakAxTaskRef) -> VfsResult<String> {
    let task = match task.upgrade() {
        Some(t) => t,
        None => return Ok("0 0 0 0 0 0 0\n".into()),
    };

    let aspace_arc = task.as_thread().proc_data.aspace();
    let mm = aspace_arc.lock();

    let mut size_pages: u64 = 0;
    let mut text_pages: u64 = 0;
    let mut data_pages: u64 = 0;
    for area in mm.areas() {
        let pages = (area.size() / PAGE_SIZE_4K) as u64;
        size_pages += pages;
        let flags = area.flags();
        if flags.contains(MappingFlags::EXECUTE) {
            text_pages += pages;
        } else if flags.contains(MappingFlags::WRITE) {
            data_pages += pages;
        }
    }

    // size resident shared text lib data dirty
    Ok(format!(
        "{size_pages} {size_pages} 0 {text_pages} 0 {data_pages} 0\n"
    ))
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
                "cgroup",
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
                let procfs_pid = self.procfs_pid;
                SimpleFile::new_regular(fs, move || {
                    let mut stat = TaskStat::from_thread(&task)?;
                    if let Some(pid) = procfs_pid {
                        stat.pid = pid;
                    }
                    Ok(format!("{stat}").into_bytes())
                })
                .into()
            }
            "statm" => {
                let task = self.task.clone();
                SimpleFile::new_regular(fs, move || render_thread_statm(&task)).into()
            }
            "status" => {
                let procfs_pid = self.procfs_pid;
                SimpleFile::new_regular(fs, move || {
                    if let Some(pid) = procfs_pid {
                        let thread = task.as_thread();
                        let cred = thread.cred();
                        let num_threads = thread.proc_data.proc.threads().len() as u32;
                        Ok(render_task_status(
                            pid,
                            pid as u64,
                            &cred,
                            num_threads,
                            task.cpumask(),
                            ax_runtime::hal::cpu_num(),
                        ))
                    } else {
                        Ok(task_status(&task))
                    }
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
            "cgroup" => SimpleFile::new_regular(fs, move || Ok(b"0::/
".to_vec())).into(),
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
        let (task, procfs_pid) = if name == "self" {
            (current().clone(), None)
        } else {
            let pid = name.parse::<u32>().map_err(|_| VfsError::NotFound)?;
            let proc_data = procfs_lookup_process(pid)?;
            let tid = proc_data
                .proc
                .threads()
                .into_iter()
                .next()
                .ok_or(VfsError::NotFound)?;
            let task = get_task(tid).map_err(|_| VfsError::NotFound)?;
            let procfs_pid =
                (procfs_visible_pid(&proc_data.proc) != proc_data.proc.pid()).then_some(pid);
            (task, procfs_pid)
        };
        let node = NodeOpsMux::Dir(SimpleDir::new_maker(
            self.0.clone(),
            Arc::new(ThreadDir {
                fs: self.0.clone(),
                task: Arc::downgrade(&task),
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

            SimpleDir::new_maker(fs.clone(), Arc::new(kernel))
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
        collect_cpu_presence, format_cpu_presence_hex, format_cpu_presence_list,
        render_task_status_fields,
    };
    use crate::task::Cred;

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

        render_task_status_fields(
            tgid,
            pid,
            &Cred::root(),
            1,
            &cpus_allowed,
            &cpus_allowed_list,
        )
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
        let status =
            render_task_status_fields(1, 1, &Cred::root(), 3, &cpus_allowed, &cpus_allowed_list);

        assert!(status.contains("Threads:\t3\n"));
        // Tab-separated, exactly as the psutil regex expects (not space).
        assert!(!status.contains("Threads: 3"));
    }
}
