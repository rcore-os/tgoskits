use alloc::{collections::btree_map::BTreeMap, vec::Vec};
use core::{
    alloc::Layout,
    any::Any,
    cmp, fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use axbacktrace::Backtrace;
use axfs_ng_vfs::VfsResult;

use crate::{task::cleanup_task_tables, vfs::DeviceOps};

static STAMPED_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum MemoryCategory {
    Known(&'static str),
    Unknown(Backtrace),
}
impl MemoryCategory {
    fn new(backtrace: &Backtrace) -> Self {
        match Self::category(backtrace) {
            Some(category) => Self::Known(category),
            None => Self::Unknown(backtrace.clone()),
        }
    }

    fn category(backtrace: &Backtrace) -> Option<&'static str> {
        for frame in backtrace.frames()? {
            let Some(func) = frame.function else {
                continue;
            };
            if func.language != Some(gimli::DW_LANG_Rust) {
                continue;
            }
            let Ok(name) = func.demangle() else {
                continue;
            };
            match name.as_ref() {
                "axfs_ng_vfs::node::dir::DirNode<M>::lookup_locked" => {
                    return Some("dentry");
                }
                "ext4_user_malloc" => {
                    return Some("ext4");
                }
                "axprocess::process::ProcessBuilder::build" => {
                    return Some("process");
                }
                _ => continue,
            }
        }

        None
    }
}
impl fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryCategory::Known(name) => write!(f, "[{name}]"),
            MemoryCategory::Unknown(backtrace) => write!(f, "{backtrace}"),
        }
    }
}

fn run_memory_leak_analysis() {
    // Wait for gc
    axtask::yield_now();
    cleanup_task_tables();

    let from = STAMPED_GENERATION.load(Ordering::SeqCst);
    let to = axalloc::current_generation();

    let mut allocations: BTreeMap<MemoryCategory, Vec<Layout>> = BTreeMap::new();
    axalloc::allocations_in(from..to, |info| {
        let category = MemoryCategory::new(&info.backtrace);
        allocations.entry(category).or_default().push(info.layout);
    });
    let mut allocations = allocations
        .into_iter()
        .map(|(category, layouts)| {
            let total_size = layouts.iter().map(|l| l.size()).sum::<usize>();
            (category, layouts, total_size)
        })
        .collect::<Vec<_>>();
    allocations.sort_by_key(|it| cmp::Reverse(it.2));
    if !allocations.is_empty() {
        warn!("===========================");
        warn!("Memory leak detected:");
        for (category, layouts, total_size) in allocations {
            warn!(
                " {} bytes, {} allocations, {:?}, {category}",
                total_size,
                layouts.len(),
                layouts[0],
            );
        }
        warn!("==========================");
    }
}

pub(crate) struct MemTrack;

impl DeviceOps for MemTrack {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if offset == 0 {
            match buf {
                b"start\n" => {
                    let generation = axalloc::current_generation();
                    STAMPED_GENERATION.store(generation, Ordering::SeqCst);
                    info!("Memory allocation generation stamped: {}", generation);
                }
                b"end\n" => {
                    run_memory_leak_analysis();
                }
                _ => {
                    warn!("Unknown command: {:?}", buf);
                }
            }
        }
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
