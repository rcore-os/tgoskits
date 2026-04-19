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
    iter,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_task::{AxCpuMask, AxTaskRef, WeakAxTaskRef, current};
use axfs_ng_vfs::{Filesystem, NodeType, VfsError, VfsResult};
use indoc::indoc;
use starry_process::Process;

use crate::{
    file::FD_TABLE,
    pseudofs::{
        DirMaker, DirMapping, NodeOpsMux, RwFile, SimpleDir, SimpleDirOps, SimpleFile,
        SimpleFileOperation, SimpleFs,
    },
    task::{AsThread, TaskStat, get_task, tasks},
};

const DUMMY_MEMINFO: &str = indoc! {"
    MemTotal:       32536204 kB
    MemFree:         5506524 kB
    MemAvailable:   18768344 kB
    Buffers:            3264 kB
    Cached:         14454588 kB
    SwapCached:            0 kB
    Active:         18229700 kB
    Inactive:        6540624 kB
    Active(anon):   11380224 kB
    Inactive(anon):        0 kB
    Active(file):    6849476 kB
    Inactive(file):  6540624 kB
    Unevictable:      930088 kB
    Mlocked:            1136 kB
    SwapTotal:       4194300 kB
    SwapFree:        4194300 kB
    Zswap:                 0 kB
    Zswapped:              0 kB
    Dirty:             47952 kB
    Writeback:             0 kB
    AnonPages:      10992512 kB
    Mapped:          1361184 kB
    Shmem:           1068056 kB
    KReclaimable:     341440 kB
    Slab:             628996 kB
    SReclaimable:     341440 kB
    SUnreclaim:       287556 kB
    KernelStack:       28704 kB
    PageTables:        85308 kB
    SecPageTables:      2084 kB
    NFS_Unstable:          0 kB
    Bounce:                0 kB
    WritebackTmp:          0 kB
    CommitLimit:    20462400 kB
    Committed_AS:   45105316 kB
    VmallocTotal:   34359738367 kB
    VmallocUsed:      205924 kB
    VmallocChunk:          0 kB
    Percpu:            23840 kB
    HardwareCorrupted:     0 kB
    AnonHugePages:   1417216 kB
    ShmemHugePages:        0 kB
    ShmemPmdMapped:        0 kB
    FileHugePages:    477184 kB
    FilePmdMapped:    288768 kB
    CmaTotal:              0 kB
    CmaFree:               0 kB
    Unaccepted:            0 kB
    HugePages_Total:       0
    HugePages_Free:        0
    HugePages_Rsvd:        0
    HugePages_Surp:        0
    Hugepagesize:       2048 kB
    Hugetlb:               0 kB
    DirectMap4k:     1739900 kB
    DirectMap2M:    31492096 kB
    DirectMap1G:     1048576 kB
"};

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
    render_task_status(
        thread.proc_data.proc.pid(),
        task.id().as_u64(),
        &cred,
        task.cpumask(),
        ax_hal::cpu_num(),
    )
}

fn render_task_status(
    tgid: u32,
    pid: u64,
    cred: &crate::task::Cred,
    cpumask: AxCpuMask,
    cpu_num: usize,
) -> String {
    let cpus_allowed = format_cpumask_hex(cpumask, cpu_num);
    let cpus_allowed_list = format_cpumask_list(cpumask, cpu_num);

    render_task_status_fields(tgid, pid, cred, &cpus_allowed, &cpus_allowed_list)
}

#[rustfmt::skip]
fn render_task_status_fields(
    tgid: u32,
    pid: u64,
    cred: &crate::task::Cred,
    cpus_allowed: &str,
    cpus_allowed_list: &str,
) -> String {
    format!(
        "Tgid:\t{tgid}\n\
        Pid:\t{pid}\n\
        Uid:\t{}\t{}\t{}\t{}\n\
        Gid:\t{}\t{}\t{}\t{}\n\
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

/// The /proc/[pid] directory
struct ThreadDir {
    fs: Arc<SimpleFs>,
    task: WeakAxTaskRef,
}

impl SimpleDirOps for ThreadDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            [
                "stat",
                "status",
                "oom_score_adj",
                "task",
                "maps",
                "mounts",
                "cmdline",
                "comm",
                "exe",
                "fd",
            ]
            .into_iter()
            .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        let task = self.task.upgrade().ok_or(VfsError::NotFound)?;
        Ok(match name {
            "stat" => SimpleFile::new_regular(fs, move || {
                Ok(format!("{}", TaskStat::from_thread(&task)?).into_bytes())
            })
            .into(),
            "status" => SimpleFile::new_regular(fs, move || Ok(task_status(&task))).into(),
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
            "maps" => SimpleFile::new_regular(fs, move || {
                Ok(indoc! {"
                    7f000000-7f001000 r--p 00000000 00:00 0          [vdso]
                    7f001000-7f003000 r-xp 00001000 00:00 0          [vdso]
                    7f003000-7f005000 r--p 00003000 00:00 0          [vdso]
                    7f005000-7f007000 rw-p 00005000 00:00 0          [vdso]
                "})
            })
            .into(),
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
            tasks()
                .into_iter()
                .map(|task| task.id().as_u64().to_string().into())
                .chain([Cow::Borrowed("self")]),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let task = if name == "self" {
            current().clone()
        } else {
            let tid = name.parse::<u32>().map_err(|_| VfsError::NotFound)?;
            get_task(tid).map_err(|_| VfsError::NotFound)?
        };
        let node = NodeOpsMux::Dir(SimpleDir::new_maker(
            self.0.clone(),
            Arc::new(ThreadDir {
                fs: self.0.clone(),
                task: Arc::downgrade(&task),
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
        "meminfo",
        SimpleFile::new_regular(fs.clone(), || Ok(DUMMY_MEMINFO)),
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
    {
        static IRQ_CNT: AtomicUsize = AtomicUsize::new(0);

        ax_task::register_timer_callback(|_| {
            IRQ_CNT.fetch_add(1, Ordering::Relaxed);
        });

        root.add(
            "interrupts",
            SimpleFile::new_regular(fs.clone(), || {
                Ok(format!("0: {}", IRQ_CNT.load(Ordering::Relaxed)))
            }),
        );
    }

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

    let proc_dir = ProcFsHandler(fs.clone());
    SimpleDir::new_maker(fs, Arc::new(proc_dir.chain(root)))
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

        render_task_status_fields(tgid, pid, &Cred::root(), &cpus_allowed, &cpus_allowed_list)
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
}
