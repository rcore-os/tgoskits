use alloc::{format, sync::Arc};
use axfs_ng_vfs::Filesystem;
use axsync::RawMutex;

use super::{
    dynamic::{DirMaker, DynamicDir, DynamicFs},
    file::SimpleFile,
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
    DynamicFs::new_with("proc".into(), 0x9fa0, builder)
}

fn builder(fs: Arc<DynamicFs>) -> DirMaker {
    let mut root = DynamicDir::builder(fs.clone());
    root.add(
        "mounts",
        SimpleFile::new(
            fs.clone(),
            || "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n",
        ),
    );
    root.add("meminfo", SimpleFile::new(fs.clone(), || DUMMY_MEMINFO));
    root.add(
        "meminfo2",
        SimpleFile::new(fs.clone(), || {
            let allocator = axalloc::global_allocator();
            format!(
                "Used Pages: {}\nAvailable Pages: {}\nUsed Memory: {} bytes\nAvailable Memory: {} bytes\n{:?}\n",
                allocator.used_pages(),
                allocator.available_pages(),
                allocator.used_bytes(),
                allocator.available_bytes(),
                allocator.usage_stats()
            )
        }),
    );
    root.build()
}
