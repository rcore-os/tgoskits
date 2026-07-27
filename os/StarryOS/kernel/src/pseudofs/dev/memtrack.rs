use alloc::{collections::btree_map::BTreeMap, vec::Vec};
use core::{
    alloc::Layout,
    any::Any,
    cmp, fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_alloc::tracking::{allocations_in, current_generation, disable_tracking, enable_tracking};
use ax_kspin::SpinNoIrq;
use axbacktrace::Backtrace;
use axfs_ng_vfs::{NodeFlags, VfsResult};

use crate::{
    mm::clear_elf_cache,
    pseudofs::DeviceOps,
    task::{cleanup_task_tables, tasks},
};

static STAMPED_GENERATION: AtomicU64 = AtomicU64::new(0);
static SAMPLE_ALLOCATION: SpinNoIrq<Option<Vec<u8>>> = SpinNoIrq::new(None);

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct AllocationBacktrace(Backtrace);

impl AllocationBacktrace {
    fn new(backtrace: &Backtrace) -> Self {
        Self(backtrace.clone().kind("alloc"))
    }
}

impl fmt::Display for AllocationBacktrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn run_memory_analysis() {
    // Wait for gc
    crate::task::yield_now();
    cleanup_task_tables()
        .unwrap_or_else(|error| panic!("task table contains an invalid user extension: {error}"));
    clear_elf_cache();

    ax_println!(
        "Alive tasks: {:?}",
        tasks()
            .unwrap_or_else(|error| panic!("failed to enumerate Starry tasks: {error}"))
            .iter()
            .map(|it| it.id_name())
            .collect::<Vec<_>>()
    );

    let from = STAMPED_GENERATION.load(Ordering::SeqCst);
    let to = current_generation();

    let mut allocations: BTreeMap<AllocationBacktrace, Vec<Layout>> = BTreeMap::new();
    allocations_in(from..to, |info| {
        let category = AllocationBacktrace::new(&info.backtrace);
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

#[inline(never)]
fn record_sample_allocation() {
    let mut sample = Vec::with_capacity(4096);
    sample.resize(4096, 0xa5);
    *SAMPLE_ALLOCATION.lock() = Some(sample);
    ax_println!("Memory allocation sample recorded");
}

#[unsafe(no_mangle)]
#[inline(never)]
fn starry_memtrack_sample_hard_leaf() -> Vec<u8> {
    let mut sample = Vec::with_capacity(8192);
    sample.resize(8192, 0x5a);
    core::hint::black_box(sample.as_ptr());
    sample
}

#[unsafe(no_mangle)]
#[inline(never)]
fn starry_memtrack_sample_hard_mid() -> Vec<u8> {
    let sample = starry_memtrack_sample_hard_leaf();
    core::hint::black_box(sample.len());
    sample
}

#[inline(never)]
fn record_hard_sample_allocation() {
    let sample = starry_memtrack_sample_hard_mid();
    *SAMPLE_ALLOCATION.lock() = Some(sample);
    ax_println!("Hard memory allocation sample recorded");
}

fn clear_sample_allocation() {
    SAMPLE_ALLOCATION.lock().take();
}

#[unsafe(no_mangle)]
#[inline(never)]
extern "C" fn starry_memtrack_symbolize_probe() {}

fn emit_symbolize_probe() {
    let probe_ip = starry_memtrack_symbolize_probe as *const () as usize;
    let backtrace = Backtrace::capture_trap(0, probe_ip, 0).kind("alloc");
    ax_println!("{backtrace}");
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
                    clear_sample_allocation();
                    let generation = current_generation();
                    STAMPED_GENERATION.store(generation, Ordering::SeqCst);
                    ax_println!("Memory allocation generation stamped: {}", generation);
                    enable_tracking();
                }
                b"sample\n" => {
                    record_sample_allocation();
                }
                b"sample_hard\n" => {
                    record_hard_sample_allocation();
                }
                b"symbolize\n" => {
                    emit_symbolize_probe();
                }
                b"end\n" => {
                    run_memory_analysis();
                    clear_sample_allocation();
                    disable_tracking();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_backtrace_formats_raw_alloc_block() {
        let category = AllocationBacktrace::new(&Backtrace::capture_trap(0, 0x1000, 0));
        let output = alloc::format!("{category}");

        assert!(output.contains("BACKTRACE_BEGIN kind=alloc"));
        assert!(output.contains("BT 0 ip=0x1001 fp=0x0"));
        assert!(output.ends_with("BACKTRACE_END\n"));
        assert!(!output.contains("Backtrace:"));
    }
}
