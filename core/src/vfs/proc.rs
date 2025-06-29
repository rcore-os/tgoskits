use core::{iter, sync::atomic::Ordering};

use alloc::{
    borrow::Cow,
    boxed::Box,
    format,
    string::ToString,
    sync::{Arc, Weak},
};
use axfs_ng_vfs::{Filesystem, VfsError, VfsResult};
use axprocess::{Process, Thread};
use axsync::RawMutex;
use axtask::current;

use crate::{
    task::{StarryTaskExt, TaskStat, ThreadData, get_thread, processes},
    vfs::simple::{
        DirMaker, DirMapping, NodeOpsMux, RwFile, SimpleDir, SimpleDirOps, SimpleFile,
        SimpleFileOperation, SimpleFs,
    },
};

const DUMMY_MEMINFO: &str = "MemTotal:       32536204 kB
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
";

pub fn new_procfs() -> Filesystem<RawMutex> {
    SimpleFs::new_with("proc".into(), 0x9fa0, builder)
}

struct ProcessTaskDir {
    fs: Arc<SimpleFs>,
    process: Weak<Process>,
}
impl SimpleDirOps<RawMutex> for ProcessTaskDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let Some(process) = self.process.upgrade() else {
            return Box::new(iter::empty());
        };
        Box::new(
            process
                .threads()
                .into_iter()
                .map(|it| it.tid().to_string().into()),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux<RawMutex>> {
        let process = self.process.upgrade().ok_or(VfsError::ENOENT)?;
        let tid = name.parse::<u32>().map_err(|_| VfsError::ENOENT)?;
        let thread = get_thread(tid).map_err(|_| VfsError::ENOENT)?;
        if thread.process().pid() != process.pid() {
            return Err(VfsError::ENOENT);
        }

        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            self.fs.clone(),
            Arc::new(ThreadDir {
                fs: self.fs.clone(),
                thread: Arc::downgrade(&thread),
            }),
        )))
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

/// The /proc/[pid] directory
struct ThreadDir {
    fs: Arc<SimpleFs>,
    thread: Weak<Thread>,
}
impl SimpleDirOps<RawMutex> for ThreadDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            ["stat", "oom_score_adj", "task"]
                .into_iter()
                .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux<RawMutex>> {
        let fs = self.fs.clone();
        let thread = self.thread.upgrade().ok_or(VfsError::ENOENT)?;
        Ok(match name {
            "stat" => SimpleFile::new(fs, move || {
                Ok(format!("{}", TaskStat::from_thread(&thread)?).into_bytes())
            })
            .into(),
            "oom_score_adj" => SimpleFile::new(
                fs,
                RwFile::new(move |req| {
                    let Some(thr_data) = thread.data::<ThreadData>() else {
                        return Err(VfsError::EBADF);
                    };
                    match req {
                        SimpleFileOperation::Read => Ok(Some(
                            thr_data
                                .oom_score_adj
                                .load(Ordering::SeqCst)
                                .to_string()
                                .into_bytes(),
                        )),
                        SimpleFileOperation::Write(data) => {
                            if !data.is_empty() {
                                let value = str::from_utf8(data)
                                    .ok()
                                    .and_then(|it| it.parse::<i32>().ok())
                                    .ok_or(VfsError::EINVAL)?;
                                thr_data.oom_score_adj.store(value, Ordering::SeqCst);
                            }
                            Ok(None)
                        }
                    }
                }),
            )
            .into(),
            "task" => SimpleDir::new_maker(
                fs.clone(),
                Arc::new(ProcessTaskDir {
                    fs,
                    process: Arc::downgrade(&thread.process()),
                }),
            )
            .into(),
            _ => return Err(VfsError::ENOENT),
        })
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

/// Handles /proc/[pid] & /proc/self
struct ProcFsHandler(Arc<SimpleFs>);
impl SimpleDirOps<RawMutex> for ProcFsHandler {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            processes()
                .into_iter()
                .map(|it| it.pid().to_string().into())
                .chain([Cow::Borrowed("self")]),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux<RawMutex>> {
        let thread = if name == "self" {
            StarryTaskExt::of(&current()).thread.clone()
        } else {
            let tid = name.parse::<u32>().map_err(|_| VfsError::ENOENT)?;
            get_thread(tid).map_err(|_| VfsError::ENOENT)?
        };
        let node = NodeOpsMux::Dir(SimpleDir::new_maker(
            self.0.clone(),
            Arc::new(ThreadDir {
                fs: self.0.clone(),
                thread: Arc::downgrade(&thread),
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
        SimpleFile::new(fs.clone(), || {
            Ok("proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n")
        }),
    );
    root.add("meminfo", SimpleFile::new(fs.clone(), || Ok(DUMMY_MEMINFO)));
    root.add(
        "meminfo2",
        SimpleFile::new(fs.clone(), || {
            let allocator = axalloc::global_allocator();
            Ok(format!(
                "Used Pages: {}\nAvailable Pages: {}\nUsed Memory: {} bytes\nAvailable Memory: {} bytes\n{:?}\n",
                allocator.used_pages(),
                allocator.available_pages(),
                allocator.used_bytes(),
                allocator.available_bytes(),
                allocator.usage_stats()
            ))
        }),
    );

    root.add("sys", {
        let mut sys = DirMapping::new();

        sys.add("kernel", {
            let mut kernel = DirMapping::new();

            kernel.add("pid_max", SimpleFile::new(fs.clone(), || Ok("32768\n")));

            SimpleDir::new_maker(fs.clone(), Arc::new(kernel))
        });

        SimpleDir::new_maker(fs.clone(), Arc::new(sys))
    });

    let proc_dir = ProcFsHandler(fs.clone());
    SimpleDir::new_maker(fs, Arc::new(proc_dir.chain(root)))
}
