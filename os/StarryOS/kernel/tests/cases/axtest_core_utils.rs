use alloc::{format, string::ToString, vec, vec::Vec};
use core::{
    hash::{Hash, Hasher},
    sync::atomic::{AtomicUsize, Ordering},
};

use axtest::prelude::*;

#[derive(Default)]
struct ByteHasher {
    value: u64,
}

impl Hasher for ByteHasher {
    fn finish(&self) -> u64 {
        self.value
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.value = self.value.wrapping_mul(131).wrapping_add(u64::from(*byte));
        }
    }
}

#[axtest::def_test]
fn cpumask_small_mask_bit_and_iteration_rules_hold() {
    use ax_cpumask::CpuMask;

    let empty = CpuMask::<8>::new();
    ax_assert!(empty.is_empty());
    ax_assert!(!empty.is_full());
    ax_assert_eq!(empty.len(), 0);
    ax_assert_eq!(empty.first_index(), None);
    ax_assert_eq!(empty.last_index(), None);
    ax_assert_eq!(empty.first_false_index(), Some(0));
    ax_assert_eq!(empty.last_false_index(), Some(7));
    ax_assert_eq!(empty.next_false_index(3), Some(4));
    ax_assert_eq!(empty.prev_false_index(3), Some(7));
    ax_assert_eq!(format!("{empty:?}"), "cpumask: []");

    let mut mask = CpuMask::<8>::new();
    ax_assert!(!mask.set(1, true));
    ax_assert!(mask.set(1, true));
    ax_assert!(!mask.set(3, true));
    ax_assert!(!mask.set(6, true));
    ax_assert!(mask.get(1));
    ax_assert!(mask.get(3));
    ax_assert!(mask.get(6));
    ax_assert_eq!(mask.len(), 3);
    ax_assert_eq!(mask.first_index(), Some(1));
    ax_assert_eq!(mask.next_index(1), Some(3));
    ax_assert_eq!(mask.next_index(3), Some(6));
    ax_assert_eq!(mask.next_index(6), None);
    ax_assert_eq!(mask.last_index(), Some(6));
    ax_assert_eq!(mask.prev_index(6), Some(3));
    ax_assert_eq!(mask.prev_index(3), Some(1));
    ax_assert_eq!(mask.prev_index(1), None);
    ax_assert_eq!(mask.first_false_index(), Some(0));
    ax_assert_eq!(mask.next_false_index(0), Some(2));
    ax_assert_eq!(mask.prev_false_index(6), Some(7));
    ax_assert_eq!(mask.last_false_index(), Some(7));

    let forward: Vec<_> = (&mask).into_iter().collect();
    ax_assert_eq!(forward, vec![1, 3, 6]);
    let reverse: Vec<_> = (&mask).into_iter().rev().collect();
    ax_assert_eq!(reverse, vec![6, 3, 1]);
    let mut iter = (&mask).into_iter();
    ax_assert_eq!(iter.next(), Some(1));
    ax_assert_eq!(iter.next_back(), Some(6));
    ax_assert_eq!(iter.next(), Some(3));
    ax_assert_eq!(iter.next_back(), Some(3));
    ax_assert_eq!(iter.next_back(), None);
    ax_assert_eq!(iter.next(), None);

    let mut inverted = mask;
    inverted.invert();
    ax_assert!(!inverted.get(1));
    ax_assert!(!inverted.get(3));
    ax_assert!(!inverted.get(6));
    ax_assert!(inverted.get(0));
    ax_assert_eq!(inverted.len(), 5);
    ax_assert_eq!(
        (&inverted).into_iter().collect::<Vec<_>>(),
        vec![0, 2, 4, 5, 7]
    );

    let full = CpuMask::<8>::full();
    ax_assert!(full.is_full());
    ax_assert_eq!(full.len(), 8);
    ax_assert_eq!(full.first_false_index(), None);
    ax_assert_eq!(full.last_false_index(), None);
    ax_assert_eq!(full.next_false_index(0), None);
    ax_assert_eq!(full.prev_false_index(7), None);
}

#[axtest::def_test]
fn cpumask_bit_ops_value_hash_and_order_rules_hold() {
    use ax_cpumask::CpuMask;

    let left = CpuMask::<8>::from_raw_bits(0b1010_1100);
    let right = CpuMask::<8>::from_raw_bits(0b0110_0110);
    ax_assert_eq!((left & right).into_value(), 0b0010_0100);
    ax_assert_eq!((left | right).into_value(), 0b1110_1110);
    ax_assert_eq!((left ^ right).into_value(), 0b1100_1010);
    ax_assert_eq!(
        (!CpuMask::<8>::from_raw_bits(0b0000_1111)).into_value(),
        0b1111_0000
    );

    let mut assigned = left;
    assigned &= right;
    ax_assert_eq!(assigned.into_value(), 0b0010_0100);
    assigned |= CpuMask::<8>::from_raw_bits(0b0000_0011);
    ax_assert_eq!(assigned.into_value(), 0b0010_0111);
    assigned ^= CpuMask::<8>::from_raw_bits(0b0010_0001);
    ax_assert_eq!(assigned.into_value(), 0b0000_0110);

    let prefix = CpuMask::<8>::mask(5);
    ax_assert_eq!(prefix.into_value(), 0b0001_1111);
    ax_assert_eq!(CpuMask::<8>::one_shot(4).into_value(), 0b0001_0000);
    ax_assert_eq!(
        CpuMask::<8>::from_value(0b0101_0101).into_value(),
        0b0101_0101
    );
    ax_assert_eq!(*CpuMask::<8>::from_raw_bits(0b11).as_value(), 0b11);
    ax_assert_eq!(CpuMask::<8>::from_raw_bits(0b11).as_bytes()[0] & 0b11, 0b11);

    let low = CpuMask::<8>::from_raw_bits(0b0000_0001);
    let high = CpuMask::<8>::from_raw_bits(0b1000_0000);
    ax_assert!(low < high);
    ax_assert_eq!(low.partial_cmp(&high), Some(core::cmp::Ordering::Less));

    let mut low_hasher = ByteHasher::default();
    low.hash(&mut low_hasher);
    let mut high_hasher = ByteHasher::default();
    high.hash(&mut high_hasher);
    ax_assert_ne!(low_hasher.finish(), high_hasher.finish());

    let large = CpuMask::<256>::from([1_u128 << 127, 1_u128 << 5]);
    ax_assert!(large.get(127));
    ax_assert!(large.get(128 + 5));
    ax_assert_eq!(large.len(), 2);
    let raw: [u128; 2] = large.into();
    ax_assert_eq!(raw, [1_u128 << 127, 1_u128 << 5]);

    let larger = CpuMask::<512>::from([0_u128, 0, 1_u128 << 63, 0]);
    ax_assert!(larger.get(256 + 63));
    let raw: [u128; 4] = larger.into();
    ax_assert_eq!(raw[2], 1_u128 << 63);
}

#[axtest::def_test]
fn kernutil_memory_descriptor_rules_hold() {
    use kernutil::memory::{MemoryDescriptor, MemoryType, PageTableInfo};
    use ranges_ext::RangeOp;

    let descriptor = MemoryDescriptor::new_with_range(0x1000..0x1800, MemoryType::Ram);
    ax_assert_eq!(descriptor.physical_start, 0x1000);
    ax_assert_eq!(descriptor.size_in_bytes, 0x800);
    ax_assert_eq!(descriptor.range(), 0x1000..0x1800);
    ax_assert_eq!(descriptor.kind(), MemoryType::Ram);
    ax_assert!(!descriptor.overwritable(&descriptor));
    ax_assert!(format!("{descriptor:?}").contains("physical_start"));

    let aligned =
        MemoryDescriptor::new_with_range_aligned(0x1234..0x2345, MemoryType::Reserved, 0x1000);
    ax_assert_eq!(aligned.physical_start, 0x1000);
    ax_assert_eq!(aligned.size_in_bytes, 0x2000);
    ax_assert_eq!(aligned.range(), 0x1000..0x3000);

    let aligned = MemoryDescriptor::new_aligned(0x1234, 0x100, MemoryType::KImage, 0x1000);
    ax_assert_eq!(aligned.physical_start, 0x1000);
    ax_assert_eq!(aligned.size_in_bytes, 0x1000);

    let free = MemoryDescriptor::new_with_range(0x4000..0x5000, MemoryType::Free);
    ax_assert!(free.overwritable(&descriptor));
    let cloned = descriptor.clone_with_range(0x2000..0x2800);
    ax_assert_eq!(cloned.physical_start, 0x2000);
    ax_assert_eq!(cloned.size_in_bytes, 0x800);
    ax_assert_eq!(cloned.memory_type, MemoryType::Ram);

    ax_assert_eq!(MemoryType::Free.to_string(), "Free  ");
    ax_assert_eq!(MemoryType::Ram.to_string(), "RAM   ");
    ax_assert_eq!(MemoryType::KImage.to_string(), "KImg  ");
    ax_assert_eq!(MemoryType::Reserved.to_string(), "Rsv   ");
    ax_assert_eq!(MemoryType::Mmio.to_string(), "MMIO  ");
    ax_assert_eq!(MemoryType::PerCpuData.to_string(), "PerCPU");
    ax_assert_eq!(MemoryType::default(), MemoryType::Free);

    let page_table = PageTableInfo::zero();
    ax_assert_eq!(page_table.asid, 0);
    ax_assert_eq!(page_table.addr, 0);
    let page_table = PageTableInfo {
        asid: 7,
        addr: 0xdead_beef,
    };
    ax_assert_eq!(page_table.asid, 7);
    ax_assert_eq!(page_table.addr, 0xdead_beef);
}

#[axtest::def_test]
fn kernutil_static_cell_initialization_and_update_rules_hold() {
    use kernutil::StaticCell;

    let initialized = StaticCell::new(vec![1_u8, 2, 3]);
    ax_assert!(initialized.is_init());
    ax_assert_eq!(initialized.len(), 3);
    ax_assert_eq!(initialized.as_slice(), [1, 2, 3]);
    let updated_len = unsafe {
        initialized.update(|values| {
            values.push(4);
            values.len()
        })
    };
    ax_assert_eq!(updated_len, 4);
    ax_assert_eq!(initialized.as_slice(), [1, 2, 3, 4]);

    let cell: StaticCell<alloc::string::String> = StaticCell::uninit();
    ax_assert!(!cell.is_init());
    cell.init("ready".to_string());
    ax_assert!(cell.is_init());
    ax_assert_eq!(cell.as_str(), "ready");
    let old = unsafe {
        cell.update(|value| {
            let old = value.clone();
            value.push_str("-updated");
            old
        })
    };
    ax_assert_eq!(old, "ready");
    ax_assert_eq!(cell.as_str(), "ready-updated");

    let single_core: StaticCell<usize> = StaticCell::uninit();
    unsafe { single_core.init_single_core(41_usize) };
    ax_assert!(single_core.is_init());
    let result = unsafe {
        single_core.update(|value| {
            *value += 1;
            *value
        })
    };
    ax_assert_eq!(result, 42);
    ax_assert_eq!(*single_core, 42);
}

#[axtest::def_test]
fn axtest_framework_descriptor_and_executor_rules_hold() {
    use axtest::{
        AxTestDescriptor, AxTestExecutionMode, AxTestExecutor, AxTestResult, InlineExecutor,
        call_module_exit, call_module_init,
    };

    fn pass() -> AxTestResult {
        AxTestResult::Ok
    }

    fn fail() -> AxTestResult {
        AxTestResult::Failed
    }

    let descriptor = AxTestDescriptor::new(
        "sample",
        "coverage::core_utils",
        pass,
        "",
        false,
        "",
        AxTestExecutionMode::Standard,
    );
    ax_assert_eq!(descriptor.name, "sample");
    ax_assert_eq!(descriptor.module, "coverage::core_utils");
    ax_assert_eq!(descriptor.executor_name, "");
    ax_assert!(!descriptor.should_panic);
    ax_assert_eq!(descriptor.ignore_reason, "");
    ax_assert_eq!(descriptor.execution_mode, AxTestExecutionMode::Standard);
    ax_assert_eq!((descriptor.test_fn)(), AxTestResult::Ok);

    let ignored = AxTestDescriptor::new(
        "ignored",
        "coverage::core_utils",
        fail,
        "",
        false,
        "not needed",
        AxTestExecutionMode::Ignore,
    );
    ax_assert_eq!(ignored.execution_mode, AxTestExecutionMode::Ignore);
    ax_assert_eq!(ignored.ignore_reason, "not needed");
    ax_assert_eq!((ignored.test_fn)(), AxTestResult::Failed);

    let custom = AxTestDescriptor::new(
        "custom",
        "coverage::core_utils",
        pass,
        "custom-executor",
        true,
        "",
        AxTestExecutionMode::Custom,
    );
    ax_assert_eq!(custom.execution_mode, AxTestExecutionMode::Custom);
    ax_assert_eq!(custom.executor_name, "custom-executor");
    ax_assert!(custom.should_panic);

    let inline = InlineExecutor;
    ax_assert_eq!(inline.name(), "inner");
    ax_assert_eq!(inline.run(pass).unwrap(), AxTestResult::Ok);
    ax_assert_eq!(inline.run(fail).unwrap(), AxTestResult::Failed);

    #[derive(Default)]
    struct CountingExecutor;

    static EXECUTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    impl AxTestExecutor for CountingExecutor {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn run(&self, test_fn: fn() -> AxTestResult) -> Result<AxTestResult, &'static str> {
            EXECUTOR_RUNS.fetch_add(1, Ordering::AcqRel);
            Ok(test_fn())
        }
    }

    let executor = CountingExecutor;
    ax_assert_eq!(executor.name(), "counting");
    ax_assert_eq!(executor.run(pass).unwrap(), AxTestResult::Ok);
    ax_assert_eq!(EXECUTOR_RUNS.load(Ordering::Acquire), 1);

    call_module_init("coverage::missing_hook", descriptor);
    call_module_exit("coverage::missing_hook", descriptor);
}
