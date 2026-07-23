use std::{cell::Cell, rc::Rc};

use ax_memory_addr::{MemoryAddr, VirtAddr, va_range};

use crate::{
    MapPrecondition, MappingBackend, MappingError, MappingOperation, MappingResult, MemoryArea,
    MemorySet,
};

const MAX_ADDR: usize = 0x10000;

type MockFlags = u8;
type MockPageTable = [MockFlags; MAX_ADDR];

macro_rules! assert_ok {
    ($expr:expr) => {
        assert!(($expr).is_ok())
    };
}

macro_rules! assert_err {
    ($expr:expr) => {
        assert!(($expr).is_err())
    };
    ($expr:expr, $err:ident) => {
        assert_eq!(($expr).err(), Some(MappingError::$err))
    };
}

#[derive(Clone)]
struct MockBackend;

type MockMemorySet = MemorySet<MockBackend>;

impl MappingBackend for MockBackend {
    type Addr = VirtAddr;
    type Flags = MockFlags;
    type PageTable = MockPageTable;
    type MappingPlan = MappingOperation<VirtAddr, MockFlags>;
    type CommitState = MockCommitState;

    fn prepare(
        &self,
        operation: Self::MappingPlan,
        pt: &mut MockPageTable,
    ) -> MappingResult<Self::MappingPlan> {
        if let MappingOperation::Map {
            start,
            size,
            precondition: MapPrecondition::Vacant,
            ..
        } = operation
            && pt
                .iter()
                .skip(start.as_usize())
                .take(size)
                .any(|entry| *entry != 0)
        {
            return Err(MappingError::AlreadyExists);
        }
        Ok(operation)
    }

    fn abort(&self, _plan: Self::MappingPlan, _pt: &mut MockPageTable) {}

    fn commit(
        &self,
        operation: Self::MappingPlan,
        pt: &mut MockPageTable,
    ) -> MappingResult<Self::CommitState> {
        commit_mock_operation(operation, pt)
    }

    fn rollback(&self, state: Self::CommitState, pt: &mut MockPageTable) -> MappingResult {
        state.restore(pt);
        Ok(())
    }

    fn finalize(&self, _state: Self::CommitState, _pt: &mut MockPageTable) {}

    fn split(&mut self, _align_diff: usize) -> Option<Self> {
        Some(self.clone())
    }
}

fn mock_map(start: VirtAddr, size: usize, flags: MockFlags, pt: &mut MockPageTable) -> bool {
    for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
        if *entry != 0 {
            return false;
        }
        *entry = flags;
    }
    true
}

fn mock_unmap(start: VirtAddr, size: usize, pt: &mut MockPageTable) -> bool {
    for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
        if *entry == 0 {
            return false;
        }
        *entry = 0;
    }
    true
}

fn mock_protect(
    start: VirtAddr,
    size: usize,
    new_flags: MockFlags,
    pt: &mut MockPageTable,
) -> bool {
    for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
        if *entry == 0 {
            return false;
        }
        *entry = new_flags;
    }
    true
}

struct MockCommitState {
    start: usize,
    entries: Vec<MockFlags>,
}

impl MockCommitState {
    fn restore(self, pt: &mut MockPageTable) {
        pt[self.start..self.start + self.entries.len()].copy_from_slice(&self.entries);
    }
}

fn commit_mock_operation(
    operation: MappingOperation<VirtAddr, MockFlags>,
    pt: &mut MockPageTable,
) -> MappingResult<MockCommitState> {
    let (start, size) = match operation {
        MappingOperation::Map { start, size, .. }
        | MappingOperation::Unmap { start, size, .. }
        | MappingOperation::Protect { start, size, .. } => (start.as_usize(), size),
    };
    let state = MockCommitState {
        start,
        entries: pt[start..start + size].to_vec(),
    };
    let success = match operation {
        MappingOperation::Map {
            start, size, flags, ..
        } => mock_map(start, size, flags, pt),
        MappingOperation::Unmap { start, size, .. } => mock_unmap(start, size, pt),
        MappingOperation::Protect {
            start,
            size,
            new_flags,
            ..
        } => mock_protect(start, size, new_flags, pt),
    };
    if success {
        Ok(state)
    } else {
        state.restore(pt);
        Err(MappingError::BadState)
    }
}

#[derive(Clone, Default)]
struct FaultControl {
    prepare_map_calls: Rc<Cell<usize>>,
    prepare_unmap_calls: Rc<Cell<usize>>,
    prepare_protect_calls: Rc<Cell<usize>>,
    map_calls: Rc<Cell<usize>>,
    unmap_calls: Rc<Cell<usize>>,
    protect_calls: Rc<Cell<usize>>,
    rollback_calls: Rc<Cell<usize>>,
    fail_prepare_map_on: Rc<Cell<Option<usize>>>,
    fail_prepare_unmap_on: Rc<Cell<Option<usize>>>,
    fail_prepare_protect_on: Rc<Cell<Option<usize>>>,
    fail_map_on: Rc<Cell<Option<usize>>>,
    fail_unmap_on: Rc<Cell<Option<usize>>>,
    fail_protect_on: Rc<Cell<Option<usize>>>,
    fail_rollback_on: Rc<Cell<Option<usize>>>,
}

impl FaultControl {
    fn should_fail(calls: &Cell<usize>, fail_on: &Cell<Option<usize>>) -> bool {
        let call = calls.get() + 1;
        calls.set(call);
        fail_on.get() == Some(call)
    }

    fn reset(&self) {
        self.prepare_map_calls.set(0);
        self.prepare_unmap_calls.set(0);
        self.prepare_protect_calls.set(0);
        self.map_calls.set(0);
        self.unmap_calls.set(0);
        self.protect_calls.set(0);
        self.rollback_calls.set(0);
        self.fail_prepare_map_on.set(None);
        self.fail_prepare_unmap_on.set(None);
        self.fail_prepare_protect_on.set(None);
        self.fail_map_on.set(None);
        self.fail_unmap_on.set(None);
        self.fail_protect_on.set(None);
        self.fail_rollback_on.set(None);
    }
}

#[derive(Clone, Default)]
struct FaultBackend(FaultControl);

impl MappingBackend for FaultBackend {
    type Addr = VirtAddr;
    type Flags = MockFlags;
    type PageTable = MockPageTable;
    type MappingPlan = MappingOperation<VirtAddr, MockFlags>;
    type CommitState = MockCommitState;

    fn prepare(
        &self,
        operation: Self::MappingPlan,
        _pt: &mut MockPageTable,
    ) -> MappingResult<Self::MappingPlan> {
        let should_fail = match operation {
            MappingOperation::Map { .. } => {
                FaultControl::should_fail(&self.0.prepare_map_calls, &self.0.fail_prepare_map_on)
            }
            MappingOperation::Unmap { .. } => FaultControl::should_fail(
                &self.0.prepare_unmap_calls,
                &self.0.fail_prepare_unmap_on,
            ),
            MappingOperation::Protect { .. } => FaultControl::should_fail(
                &self.0.prepare_protect_calls,
                &self.0.fail_prepare_protect_on,
            ),
        };
        if should_fail {
            Err(MappingError::NoMemory)
        } else {
            Ok(operation)
        }
    }

    fn abort(&self, _plan: Self::MappingPlan, _pt: &mut MockPageTable) {}

    fn commit(
        &self,
        operation: Self::MappingPlan,
        pt: &mut MockPageTable,
    ) -> MappingResult<Self::CommitState> {
        let should_fail = match operation {
            MappingOperation::Map { .. } => {
                FaultControl::should_fail(&self.0.map_calls, &self.0.fail_map_on)
            }
            MappingOperation::Unmap { .. } => {
                FaultControl::should_fail(&self.0.unmap_calls, &self.0.fail_unmap_on)
            }
            MappingOperation::Protect { .. } => {
                FaultControl::should_fail(&self.0.protect_calls, &self.0.fail_protect_on)
            }
        };
        if should_fail {
            Err(MappingError::BadState)
        } else {
            commit_mock_operation(operation, pt)
        }
    }

    fn rollback(&self, state: Self::CommitState, pt: &mut MockPageTable) -> MappingResult {
        if FaultControl::should_fail(&self.0.rollback_calls, &self.0.fail_rollback_on) {
            return Err(MappingError::BadState);
        }
        state.restore(pt);
        Ok(())
    }

    fn finalize(&self, _state: Self::CommitState, _pt: &mut MockPageTable) {}

    fn split(&mut self, _align_diff: usize) -> Option<Self> {
        Some(self.clone())
    }
}

type FaultMemorySet = MemorySet<FaultBackend>;

struct CloneCounterBackend {
    clone_count: Rc<Cell<usize>>,
}

impl Clone for CloneCounterBackend {
    fn clone(&self) -> Self {
        self.clone_count.set(self.clone_count.get() + 1);
        Self {
            clone_count: self.clone_count.clone(),
        }
    }
}

impl MappingBackend for CloneCounterBackend {
    type Addr = VirtAddr;
    type Flags = MockFlags;
    type PageTable = MockPageTable;
    type MappingPlan = MappingOperation<VirtAddr, MockFlags>;
    type CommitState = MockCommitState;

    fn prepare(
        &self,
        operation: Self::MappingPlan,
        _pt: &mut Self::PageTable,
    ) -> MappingResult<Self::MappingPlan> {
        Ok(operation)
    }

    fn abort(&self, _plan: Self::MappingPlan, _pt: &mut Self::PageTable) {}

    fn commit(
        &self,
        operation: Self::MappingPlan,
        pt: &mut Self::PageTable,
    ) -> MappingResult<Self::CommitState> {
        commit_mock_operation(operation, pt)
    }

    fn rollback(&self, state: Self::CommitState, pt: &mut Self::PageTable) -> MappingResult {
        state.restore(pt);
        Ok(())
    }

    fn finalize(&self, _state: Self::CommitState, _pt: &mut Self::PageTable) {}

    fn split(&mut self, _align_diff: usize) -> Option<Self> {
        Some(self.clone())
    }
}

#[derive(Clone)]
struct UnsplittableBackend;

impl MappingBackend for UnsplittableBackend {
    type Addr = VirtAddr;
    type Flags = MockFlags;
    type PageTable = MockPageTable;
    type MappingPlan = MappingOperation<VirtAddr, MockFlags>;
    type CommitState = MockCommitState;

    fn prepare(
        &self,
        operation: Self::MappingPlan,
        _pt: &mut Self::PageTable,
    ) -> MappingResult<Self::MappingPlan> {
        Ok(operation)
    }

    fn abort(&self, _plan: Self::MappingPlan, _pt: &mut Self::PageTable) {}

    fn commit(
        &self,
        operation: Self::MappingPlan,
        pt: &mut Self::PageTable,
    ) -> MappingResult<Self::CommitState> {
        commit_mock_operation(operation, pt)
    }

    fn rollback(&self, state: Self::CommitState, pt: &mut Self::PageTable) -> MappingResult {
        state.restore(pt);
        Ok(())
    }

    fn finalize(&self, _state: Self::CommitState, _pt: &mut Self::PageTable) {}

    fn split(&mut self, _align_diff: usize) -> Option<Self> {
        None
    }
}

#[test]
fn unsplittable_backend_reports_failure_without_panicking() {
    let mut area = MemoryArea::new(VirtAddr::from(0x1000), 0x2000, 1, UnsplittableBackend);

    assert!(area.split(VirtAddr::from(0x2000)).is_none());
    assert_eq!(area.va_range(), va_range!(0x1000..0x3000));
}

#[test]
fn failed_metadata_split_preserves_the_original_area() {
    let mut set = MemorySet::new();
    let mut page_table = [0; MAX_ADDR];
    set.map(
        MemoryArea::new(VirtAddr::from(0x1000), 0x3000, 1, UnsplittableBackend),
        &mut page_table,
        false,
    )
    .unwrap();

    assert_eq!(
        set.unmap_metadata(VirtAddr::from(0x2000), 0x1000),
        Err(MappingError::BadState)
    );
    let areas = set.iter().map(|area| area.va_range()).collect::<Vec<_>>();
    assert_eq!(areas, [va_range!(0x1000..0x4000)]);
}

fn fault_set_snapshot(set: &FaultMemorySet) -> Vec<(usize, usize, MockFlags)> {
    set.iter()
        .map(|area| (area.start().as_usize(), area.size(), area.flags()))
        .collect()
}

fn mapped_fault_set() -> (FaultMemorySet, MockPageTable, FaultControl) {
    let control = FaultControl::default();
    let backend = FaultBackend(control.clone());
    let mut set = FaultMemorySet::new();
    let mut pt = [0; MAX_ADDR];
    assert_ok!(set.map(
        MemoryArea::new(0x1000.into(), 0x1000, 1, backend.clone()),
        &mut pt,
        false,
    ));
    assert_ok!(set.map(
        MemoryArea::new(0x3000.into(), 0x1000, 1, backend),
        &mut pt,
        false,
    ));
    control.reset();
    (set, pt, control)
}

fn mapped_fault_set_three() -> (FaultMemorySet, MockPageTable, FaultControl) {
    let control = FaultControl::default();
    let backend = FaultBackend(control.clone());
    let mut set = FaultMemorySet::new();
    let mut pt = [0; MAX_ADDR];
    for start in [0x1000, 0x3000, 0x5000] {
        assert_ok!(set.map(
            MemoryArea::new(start.into(), 0x1000, 1, backend.clone()),
            &mut pt,
            false,
        ));
    }
    control.reset();
    (set, pt, control)
}

#[test]
fn prepare_failure_at_each_unmap_backend_preserves_the_transaction() {
    for fail_on in 1..=3 {
        let (mut set, mut pt, control) = mapped_fault_set_three();
        let areas_before = fault_set_snapshot(&set);
        let pt_before = pt;
        control.fail_prepare_unmap_on.set(Some(fail_on));

        assert_err!(set.unmap(0x1000.into(), 0x5000, &mut pt), NoMemory);
        assert_eq!(fault_set_snapshot(&set), areas_before);
        assert_eq!(pt, pt_before);
    }
}

#[test]
fn commit_failure_at_each_unmap_backend_rolls_back_the_transaction() {
    for fail_on in 1..=3 {
        let (mut set, mut pt, control) = mapped_fault_set_three();
        let areas_before = fault_set_snapshot(&set);
        let pt_before = pt;
        control.fail_unmap_on.set(Some(fail_on));

        assert_err!(set.unmap(0x1000.into(), 0x5000, &mut pt), BadState);
        assert_eq!(fault_set_snapshot(&set), areas_before);
        assert_eq!(pt, pt_before);
    }
}

#[test]
fn prepare_failure_at_each_protect_backend_preserves_the_transaction() {
    for fail_on in 1..=3 {
        let (mut set, mut pt, control) = mapped_fault_set_three();
        let areas_before = fault_set_snapshot(&set);
        let pt_before = pt;
        control.fail_prepare_protect_on.set(Some(fail_on));

        assert_err!(
            set.protect(0x1000.into(), 0x5000, |_| Some(2), &mut pt),
            NoMemory
        );
        assert_eq!(fault_set_snapshot(&set), areas_before);
        assert_eq!(pt, pt_before);
    }
}

#[test]
fn commit_failure_at_each_protect_backend_rolls_back_the_transaction() {
    for fail_on in 1..=3 {
        let (mut set, mut pt, control) = mapped_fault_set_three();
        let areas_before = fault_set_snapshot(&set);
        let pt_before = pt;
        control.fail_protect_on.set(Some(fail_on));

        assert_err!(
            set.protect(0x1000.into(), 0x5000, |_| Some(2), &mut pt),
            BadState
        );
        assert_eq!(fault_set_snapshot(&set), areas_before);
        assert_eq!(pt, pt_before);
    }
}

#[test]
fn one_rollback_failure_does_not_skip_remaining_rollback_attempts() {
    let (mut set, mut pt, control) = mapped_fault_set_three();
    control.fail_protect_on.set(Some(3));
    control.fail_rollback_on.set(Some(1));

    assert_err!(
        set.protect(0x1000.into(), 0x5000, |_| Some(2), &mut pt),
        BadState
    );
    assert_eq!(control.rollback_calls.get(), 2);
}

#[test]
fn failed_unmap_rolls_back_all_vmas_and_page_table_entries() {
    let (mut set, mut pt, control) = mapped_fault_set();
    let areas_before = fault_set_snapshot(&set);
    let pt_before = pt;
    control.fail_unmap_on.set(Some(2));

    assert_err!(set.unmap(0x1000.into(), 0x3000, &mut pt), BadState);
    assert_eq!(fault_set_snapshot(&set), areas_before);
    assert_eq!(pt, pt_before);
}

#[test]
fn failed_protect_rolls_back_all_vmas_and_page_table_entries() {
    let (mut set, mut pt, control) = mapped_fault_set();
    let areas_before = fault_set_snapshot(&set);
    let pt_before = pt;
    control.fail_protect_on.set(Some(2));

    assert_err!(
        set.protect(0x1000.into(), 0x3000, |_| Some(2), &mut pt),
        BadState
    );
    assert_eq!(fault_set_snapshot(&set), areas_before);
    assert_eq!(pt, pt_before);
}

#[test]
fn failed_replacement_map_restores_overlapped_mapping() {
    let (mut set, mut pt, control) = mapped_fault_set();
    let areas_before = fault_set_snapshot(&set);
    let pt_before = pt;
    control.fail_map_on.set(Some(1));

    assert_err!(
        set.map(
            MemoryArea::new(0x1000.into(), 0x3000, 2, FaultBackend(control.clone())),
            &mut pt,
            true,
        ),
        BadState
    );
    assert_eq!(fault_set_snapshot(&set), areas_before);
    assert_eq!(pt, pt_before);
}

#[test]
fn failed_explicit_replacement_restores_the_full_replacement_range() {
    let (mut set, mut pt, control) = mapped_fault_set();
    let areas_before = fault_set_snapshot(&set);
    let pt_before = pt;
    control.fail_map_on.set(Some(1));

    assert_err!(
        set.replace(
            va_range!(0x1000..0x4000),
            MemoryArea::new(0x1000.into(), 0x1000, 2, FaultBackend(control.clone())),
            &mut pt,
        ),
        BadState
    );
    assert_eq!(fault_set_snapshot(&set), areas_before);
    assert_eq!(pt, pt_before);
}

#[test]
fn explicit_replacement_removes_the_tail_outside_the_new_area() {
    let (mut set, mut pt, _) = mapped_fault_set();

    assert_ok!(set.replace(
        va_range!(0x1000..0x4000),
        MemoryArea::new(0x1000.into(), 0x1000, 2, FaultBackend::default()),
        &mut pt,
    ));
    assert_eq!(fault_set_snapshot(&set), vec![(0x1000, 0x1000, 2)]);
    assert!(pt[0x1000..0x2000].iter().all(|&entry| entry == 2));
    assert!(pt[0x2000..0x4000].iter().all(|&entry| entry == 0));
}

#[test]
fn replacement_preflight_accepts_mappings_removed_by_the_same_transaction() {
    let (mut set, mut pt, _) = mapped_fault_set();

    assert_ok!(set.map(
        MemoryArea::new(0x1000.into(), 0x3000, 2, FaultBackend::default()),
        &mut pt,
        true,
    ));
    assert_eq!(fault_set_snapshot(&set), vec![(0x1000, 0x3000, 2)]);
    assert!(pt[0x1000..0x4000].iter().all(|&entry| entry == 2));
}

#[test]
fn protect_does_not_clone_unrelated_vma_backends() {
    let counters = [
        Rc::new(Cell::new(0)),
        Rc::new(Cell::new(0)),
        Rc::new(Cell::new(0)),
    ];
    let mut set = MemorySet::<CloneCounterBackend>::new();
    let mut pt = [0; MAX_ADDR];
    for (start, clone_count) in [0x1000, 0x3000, 0x5000].into_iter().zip(&counters) {
        assert_ok!(set.map(
            MemoryArea::new(
                start.into(),
                0x1000,
                1,
                CloneCounterBackend {
                    clone_count: clone_count.clone(),
                },
            ),
            &mut pt,
            false,
        ));
    }
    for counter in &counters {
        counter.set(0);
    }

    assert_ok!(set.protect(0x3000.into(), 0x1000, |_| Some(2), &mut pt));

    assert_eq!(counters[0].get(), 0);
    assert!(counters[1].get() > 0);
    assert_eq!(counters[2].get(), 0);
}

fn dump_memory_set(set: &MockMemorySet) {
    use std::sync::Mutex;
    static DUMP_LOCK: Mutex<()> = Mutex::new(());

    let _lock = DUMP_LOCK.lock().unwrap();
    println!("Number of areas: {}", set.len());
    for area in set.iter() {
        println!("{:?}", area);
    }
}

#[test]
fn test_map_unmap() {
    let mut set = MockMemorySet::new();
    let mut pt = [0; MAX_ADDR];

    // Map [0, 0x1000), [0x2000, 0x3000), [0x4000, 0x5000), ...
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.map(
            MemoryArea::new(start.into(), 0x1000, 1, MockBackend),
            &mut pt,
            false,
        ));
    }
    // Map [0x1000, 0x2000), [0x3000, 0x4000), [0x5000, 0x6000), ...
    for start in (0x1000..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.map(
            MemoryArea::new(start.into(), 0x1000, 2, MockBackend),
            &mut pt,
            false,
        ));
    }
    dump_memory_set(&set);
    assert_eq!(set.len(), 16);
    for &e in &pt[0..MAX_ADDR] {
        assert!(e == 1 || e == 2);
    }

    // Found [0x4000, 0x5000), flags = 1.
    let area = set.find(0x4100.into()).unwrap();
    assert_eq!(area.start(), 0x4000.into());
    assert_eq!(area.end(), 0x5000.into());
    assert_eq!(area.flags(), 1);
    assert_eq!(pt[0x4200], 1);

    // The area [0x4000, 0x8000) is already mapped, map returns an error.
    assert_err!(
        set.map(
            MemoryArea::new(0x4000.into(), 0x4000, 3, MockBackend),
            &mut pt,
            false
        ),
        AlreadyExists
    );
    // Unmap overlapped areas before adding the new mapping [0x4000, 0x8000).
    assert_ok!(set.map(
        MemoryArea::new(0x4000.into(), 0x4000, 3, MockBackend),
        &mut pt,
        true
    ));
    dump_memory_set(&set);
    assert_eq!(set.len(), 13);

    // Found [0x4000, 0x8000), flags = 3.
    let area = set.find(0x4100.into()).unwrap();
    assert_eq!(area.start(), 0x4000.into());
    assert_eq!(area.end(), 0x8000.into());
    assert_eq!(area.flags(), 3);
    for &e in &pt[0x4000..0x8000] {
        assert_eq!(e, 3);
    }

    // Unmap areas in the middle.
    assert_ok!(set.unmap(0x4000.into(), 0x8000, &mut pt));
    assert_eq!(set.len(), 8);
    // Unmap the remaining areas, including the unmapped ranges.
    assert_ok!(set.unmap(0.into(), MAX_ADDR * 2, &mut pt));
    assert_eq!(set.len(), 0);
    for &e in &pt[0..MAX_ADDR] {
        assert_eq!(e, 0);
    }
}

#[test]
fn map_metadata_does_not_touch_preinstalled_page_table_entries() {
    let mut set = MockMemorySet::new();
    let mut pt = [0; MAX_ADDR];
    pt[0x1000] = 7;

    assert_ok!(set.map_metadata(MemoryArea::new(0x1000.into(), 0x1000, 7, MockBackend,)));

    assert_eq!(pt[0x1000], 7);
    assert_eq!(set.find(0x1000.into()).map(MemoryArea::flags), Some(7));
    assert_eq!(
        set.map_metadata(MemoryArea::new(0x1000.into(), 0x1000, 7, MockBackend,)),
        Err(MappingError::AlreadyExists),
    );
}

#[test]
fn test_unmap_split() {
    let mut set = MockMemorySet::new();
    let mut pt = [0; MAX_ADDR];

    // Map [0, 0x1000), [0x2000, 0x3000), [0x4000, 0x5000), ...
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.map(
            MemoryArea::new(start.into(), 0x1000, 1, MockBackend),
            &mut pt,
            false,
        ));
    }
    assert_eq!(set.len(), 8);

    // Unmap [0xc00, 0x2400), [0x2c00, 0x4400), [0x4c00, 0x6400), ...
    // The areas are shrinked at the left and right boundaries.
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.unmap((start + 0xc00).into(), 0x1800, &mut pt));
    }
    dump_memory_set(&set);
    assert_eq!(set.len(), 8);

    for area in set.iter() {
        if area.start().as_usize() == 0 {
            assert_eq!(area.size(), 0xc00);
        } else {
            assert_eq!(area.start().align_offset_4k(), 0x400);
            assert_eq!(area.end().align_offset_4k(), 0xc00);
            assert_eq!(area.size(), 0x800);
        }
        for &e in &pt[area.start().as_usize()..area.end().as_usize()] {
            assert_eq!(e, 1);
        }
    }

    // Unmap [0x800, 0x900), [0x2800, 0x2900), [0x4800, 0x4900), ...
    // The areas are split into two areas.
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.unmap((start + 0x800).into(), 0x100, &mut pt));
    }
    dump_memory_set(&set);
    assert_eq!(set.len(), 16);

    for area in set.iter() {
        let off = area.start().align_offset_4k();
        if off == 0 {
            assert_eq!(area.size(), 0x800);
        } else if off == 0x400 {
            assert_eq!(area.size(), 0x400);
        } else if off == 0x900 {
            assert_eq!(area.size(), 0x300);
        } else {
            unreachable!();
        }
        for &e in &pt[area.start().as_usize()..area.end().as_usize()] {
            assert_eq!(e, 1);
        }
    }
    let mut iter = set.iter();
    while let Some(area) = iter.next() {
        if let Some(next) = iter.next() {
            for &e in &pt[area.end().as_usize()..next.start().as_usize()] {
                assert_eq!(e, 0);
            }
        }
    }
    drop(iter);

    // Unmap all areas.
    assert_ok!(set.unmap(0.into(), MAX_ADDR, &mut pt));
    assert_eq!(set.len(), 0);
    for &e in &pt[0..MAX_ADDR] {
        assert_eq!(e, 0);
    }
}

#[test]
fn test_protect() {
    let mut set = MockMemorySet::new();
    let mut pt = [0; MAX_ADDR];
    let update_flags = |new_flags: MockFlags| {
        move |old_flags: MockFlags| -> Option<MockFlags> {
            if (old_flags & 0x7) == (new_flags & 0x7) {
                return None;
            }
            let flags = (new_flags & 0x7) | (old_flags & !0x7);
            Some(flags)
        }
    };

    // Map [0, 0x1000), [0x2000, 0x3000), [0x4000, 0x5000), ...
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.map(
            MemoryArea::new(start.into(), 0x1000, 0x7, MockBackend),
            &mut pt,
            false,
        ));
    }
    assert_eq!(set.len(), 8);

    // Protect [0xc00, 0x2400), [0x2c00, 0x4400), [0x4c00, 0x6400), ...
    // The areas are split into two areas.
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.protect((start + 0xc00).into(), 0x1800, update_flags(0x1), &mut pt));
    }
    dump_memory_set(&set);
    assert_eq!(set.len(), 23);

    for area in set.iter() {
        let off = area.start().align_offset_4k();
        if area.start().as_usize() == 0 {
            assert_eq!(area.size(), 0xc00);
            assert_eq!(area.flags(), 0x7);
        } else if off == 0 {
            assert_eq!(area.size(), 0x400);
            assert_eq!(area.flags(), 0x1);
        } else if off == 0x400 {
            assert_eq!(area.size(), 0x800);
            assert_eq!(area.flags(), 0x7);
        } else if off == 0xc00 {
            assert_eq!(area.size(), 0x400);
            assert_eq!(area.flags(), 0x1);
        }
    }

    // Protect [0x800, 0x900), [0x2800, 0x2900), [0x4800, 0x4900), ...
    // The areas are split into three areas.
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.protect((start + 0x800).into(), 0x100, update_flags(0x13), &mut pt));
    }
    dump_memory_set(&set);
    assert_eq!(set.len(), 39);

    for area in set.iter() {
        let off = area.start().align_offset_4k();
        if area.start().as_usize() == 0 {
            assert_eq!(area.size(), 0x800);
            assert_eq!(area.flags(), 0x7);
        } else if off == 0 {
            assert_eq!(area.size(), 0x400);
            assert_eq!(area.flags(), 0x1);
        } else if off == 0x400 {
            assert_eq!(area.size(), 0x400);
            assert_eq!(area.flags(), 0x7);
        } else if off == 0x800 {
            assert_eq!(area.size(), 0x100);
            assert_eq!(area.flags(), 0x3);
        } else if off == 0x900 {
            assert_eq!(area.size(), 0x300);
            assert_eq!(area.flags(), 0x7);
        } else if off == 0xc00 {
            assert_eq!(area.size(), 0x400);
            assert_eq!(area.flags(), 0x1);
        }
    }

    // Test skip [0x880, 0x900), [0x2880, 0x2900), [0x4880, 0x4900), ...
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.protect((start + 0x880).into(), 0x80, update_flags(0x3), &mut pt));
    }
    assert_eq!(set.len(), 39);

    // Unmap all areas.
    assert_ok!(set.unmap(0.into(), MAX_ADDR, &mut pt));
    assert_eq!(set.len(), 0);
    for &e in &pt[0..MAX_ADDR] {
        assert_eq!(e, 0);
    }
}

#[test]
fn test_find_free_area() {
    let mut set = MockMemorySet::new();
    let mut pt = [0; MAX_ADDR];

    // Map [0, 0x1000), [0x2000, 0x3000), ..., [0xe000, 0xf000)
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.map(
            MemoryArea::new(start.into(), 0x1000, 1, MockBackend),
            &mut pt,
            false,
        ));
    }

    let addr = set.find_free_area(0.into(), 0x1000, va_range!(0..MAX_ADDR), 1);
    assert_eq!(addr, Some(0x1000.into()));

    let addr = set.find_free_area(0x800.into(), 0x800, va_range!(0..MAX_ADDR), 0x800);
    assert_eq!(addr, Some(0x1000.into()));

    let addr = set.find_free_area(0x1800.into(), 0x800, va_range!(0..MAX_ADDR), 0x800);
    assert_eq!(addr, Some(0x1800.into()));

    let addr = set.find_free_area(0x1800.into(), 0x1000, va_range!(0..MAX_ADDR), 0x1000);
    assert_eq!(addr, Some(0x3000.into()));

    let addr = set.find_free_area(0x2000.into(), 0x1000, va_range!(0..MAX_ADDR), 0x1000);
    assert_eq!(addr, Some(0x3000.into()));

    let addr = set.find_free_area(0xf000.into(), 0x1000, va_range!(0..MAX_ADDR), 0x1000);
    assert_eq!(addr, Some(0xf000.into()));

    let addr = set.find_free_area(0xf001.into(), 0x1000, va_range!(0..MAX_ADDR), 0x1000);
    assert_eq!(addr, None);
}
