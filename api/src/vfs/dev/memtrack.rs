use alloc::{collections::btree_map::BTreeMap, vec::Vec};
use core::{
    alloc::Layout,
    any::Any,
    cmp, fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use axbacktrace::Backtrace;
use axfs_ng_vfs::{NodeFlags, VfsResult};
use starry_core::{
    mm::clear_elf_cache,
    task::{cleanup_task_tables, tasks},
};

use crate::vfs::DeviceOps;

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
        for (_, frame) in backtrace.frames()? {
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
                "starry_core::mm::ElfLoader::load" => {
                    return Some("elf cache");
                }
                "starry_core::task::ProcessData::new" => {
                    return Some("process data");
                }
                "starry_process::process::Process::new" => {
                    return Some("process");
                }
                "starry_process::process_group::ProcessGroup::new" => {
                    return Some("process group");
                }
                "axfs::fs::ext4::inode::Inode::new" => {
                    return Some("ext4 inode");
                }
                "axfs::highlevel::file::CachedFile::get_or_create"
                | "axfs::highlevel::file::CachedFile::page_or_insert" => {
                    return Some("cached file");
                }
                "axtask::timers::set_alarm_wakeup" => {
                    return Some("timer");
                }
                "axfs_ng_vfs::node::dir::DirNode::lookup_locked"
                | "axfs_ng_vfs::node::dir::DirNode::create_locked" => {
                    return Some("dentry");
                }
                "ext4_user_malloc" => {
                    return Some("lwext4");
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

fn run_memory_analysis() {
    // Wait for gc
    axtask::yield_now();
    cleanup_task_tables();
    clear_elf_cache();

    ax_println!(
        "Alive tasks: {:?}",
        tasks().iter().map(|it| it.id_name()).collect::<Vec<_>>()
    );

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
        ax_println!("===========================");
        ax_println!("Memory usage:");
        for (category, layouts, total_size) in allocations {
            ax_println!(
                " {} bytes, {} allocations, {:?}, {category}",
                total_size,
                layouts.len(),
                layouts[0],
            );
        }
        ax_println!("==========================");
    }
}

pub(crate) struct MemTrack;

impl DeviceOps for MemTrack {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if offset == 0 && !buf.is_empty() {
            match buf {
                b"start\n" => {
                    let generation = axalloc::current_generation();
                    STAMPED_GENERATION.store(generation, Ordering::SeqCst);
                    ax_println!("Memory allocation generation stamped: {}", generation);
                    axalloc::enable_tracking();
                }
                b"end\n" => {
                    run_memory_analysis();
                    axalloc::disable_tracking();
                }
                _ => {}
            }
        }
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}
