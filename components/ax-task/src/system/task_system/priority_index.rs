//! Root-domain priority indexes for RT and Deadline placement.

use alloc::{vec, vec::Vec};
use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::RtPriority;

#[cfg(any(test, all(axtest, feature = "axtest")))]
std::thread_local! {
    static PRIORITY_INDEX_LOOKUPS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
    static DEADLINE_INDEX_PUBLICATIONS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(super) fn reset_priority_index_lookups() {
    PRIORITY_INDEX_LOOKUPS.set(0);
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(super) fn priority_index_lookups() -> usize {
    PRIORITY_INDEX_LOOKUPS.get()
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(super) fn reset_deadline_index_publications() {
    DEADLINE_INDEX_PUBLICATIONS.set(0);
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(super) fn deadline_index_publications() -> usize {
    DEADLINE_INDEX_PUBLICATIONS.get()
}

const RT_NORMAL_LEVEL: u8 = 0;
// Linux CPUPRI_HIGHER: CPUs with runnable DL work are never RT wake targets.
const RT_HIGHER_LEVEL: u8 = 100;
const RT_LEVEL_COUNT: usize = 101;
const RT_OFFLINE_LEVEL: u8 = u8::MAX;
const DEADLINE_CPU_OFFLINE: u8 = 0;
const DEADLINE_CPU_FREE: u8 = 1;
const DEADLINE_CPU_BUSY: u8 = 2;

/// Derived root-domain indexes used by class-specific wake placement.
///
/// `RtCpuPriorityIndex` mirrors Linux cpupri's priority buckets. The Deadline
/// side mirrors cpudl's free-CPU set and maximum absolute-deadline heap. Neither
/// object owns runnable state; stale observations are legal placement hints and
/// the target rq transaction remains the correctness boundary.
#[derive(Debug)]
pub(super) struct RootDomainPriorityIndex {
    rt: RtCpuPriorityIndex,
    deadline: IrqTicketLock<DeadlineCpuHeap>,
    published_deadline: Vec<DeadlineCpuPublication>,
}

impl RootDomainPriorityIndex {
    pub(super) fn new(cpu_count: usize) -> Self {
        Self {
            rt: RtCpuPriorityIndex::new(cpu_count),
            deadline: IrqTicketLock::new(DeadlineCpuHeap::new(cpu_count)),
            published_deadline: (0..cpu_count)
                .map(|_| DeadlineCpuPublication::new())
                .collect(),
        }
    }

    pub(super) fn publish_run_queue(
        &self,
        cpu: CpuId,
        highest_rt_priority: Option<u8>,
        earliest_deadline: Option<u64>,
        online: bool,
    ) {
        self.rt.publish(
            cpu,
            online,
            highest_rt_priority,
            earliest_deadline.is_some(),
        );
        self.publish_deadline(cpu, online, earliest_deadline);
    }

    pub(super) fn publish_offline(&self, cpu: CpuId) {
        self.rt.publish(cpu, false, None, false);
        self.publish_deadline(cpu, false, None);
    }

    fn publish_deadline(&self, cpu: CpuId, online: bool, earliest_deadline: Option<u64>) {
        let Some(published) = self.published_deadline.get(cpu.as_usize()) else {
            return;
        };
        if published.matches(online, earliest_deadline) {
            return;
        }

        self.deadline
            .lock(crate::runtime::IrqGuardSource::RootDeadlineIndexTicket)
            .publish(cpu, online, earliest_deadline);
        published.record(online, earliest_deadline);
        #[cfg(any(test, all(axtest, feature = "axtest")))]
        DEADLINE_INDEX_PUBLICATIONS.set(DEADLINE_INDEX_PUBLICATIONS.get().saturating_add(1));
    }

    pub(super) fn has_multiple_online_cpus(&self) -> bool {
        self.rt
            .levels
            .iter()
            .filter(|level| level.load(Ordering::Acquire) != RT_OFFLINE_LEVEL)
            .take(2)
            .count()
            > 1
    }

    pub(super) fn find_lowest_rt_cpu(
        &self,
        priority: RtPriority,
        affinity: &CpuSet,
        preferred: Option<CpuId>,
        mut accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        #[cfg(any(test, all(axtest, feature = "axtest")))]
        PRIORITY_INDEX_LOOKUPS.set(PRIORITY_INDEX_LOOKUPS.get().saturating_add(1));
        self.rt
            .find_lower(priority.get(), affinity, preferred, &mut accepts)
    }

    pub(super) fn find_later_deadline_cpu(
        &self,
        absolute_deadline_ns: u64,
        affinity: &CpuSet,
        preferred: Option<CpuId>,
        accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        #[cfg(any(test, all(axtest, feature = "axtest")))]
        PRIORITY_INDEX_LOOKUPS.set(PRIORITY_INDEX_LOOKUPS.get().saturating_add(1));
        self.deadline
            .lock(crate::runtime::IrqGuardSource::RootDeadlineIndexTicket)
            .find_later(absolute_deadline_ns, affinity, preferred, accepts)
    }
}

/// Lockless mirror of one CPU's last committed cpudl state.
///
/// Writers are serialized by that CPU's rq ownership (including the final
/// hotplug transition). The mirror is recorded only after the heap mutation,
/// so an equal observation may skip the heap lock without hiding an
/// unpublished transition.
#[derive(Debug)]
struct DeadlineCpuPublication {
    state: AtomicU8,
    absolute_deadline_ns: AtomicU64,
}

impl DeadlineCpuPublication {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(DEADLINE_CPU_OFFLINE),
            absolute_deadline_ns: AtomicU64::new(0),
        }
    }

    fn matches(&self, online: bool, earliest_deadline: Option<u64>) -> bool {
        let expected = Self::state_for(online, earliest_deadline);
        self.state.load(Ordering::Acquire) == expected
            && (expected != DEADLINE_CPU_BUSY
                || self.absolute_deadline_ns.load(Ordering::Acquire)
                    == earliest_deadline.expect("busy cpudl state must carry a deadline"))
    }

    fn record(&self, online: bool, earliest_deadline: Option<u64>) {
        let state = Self::state_for(online, earliest_deadline);
        if let Some(deadline) = earliest_deadline {
            self.absolute_deadline_ns.store(deadline, Ordering::Release);
        }
        self.state.store(state, Ordering::Release);
    }

    const fn state_for(online: bool, earliest_deadline: Option<u64>) -> u8 {
        if !online {
            DEADLINE_CPU_OFFLINE
        } else if earliest_deadline.is_some() {
            DEADLINE_CPU_BUSY
        } else {
            DEADLINE_CPU_FREE
        }
    }
}

#[derive(Debug)]
struct RtCpuPriorityIndex {
    words_per_level: usize,
    levels: Vec<AtomicU8>,
    buckets: Vec<AtomicUsize>,
}

impl RtCpuPriorityIndex {
    fn new(cpu_count: usize) -> Self {
        let words_per_level = cpu_count.div_ceil(usize::BITS as usize);
        let levels = (0..cpu_count)
            .map(|_| AtomicU8::new(RT_OFFLINE_LEVEL))
            .collect();
        let buckets = (0..RT_LEVEL_COUNT.saturating_mul(words_per_level))
            .map(|_| AtomicUsize::new(0))
            .collect();
        Self {
            words_per_level,
            levels,
            buckets,
        }
    }

    fn publish(
        &self,
        cpu: CpuId,
        online: bool,
        highest_rt_priority: Option<u8>,
        has_deadline_work: bool,
    ) {
        let Some(level) = self.levels.get(cpu.as_usize()) else {
            return;
        };
        let new_level = if online && has_deadline_work {
            RT_HIGHER_LEVEL
        } else if online {
            highest_rt_priority.unwrap_or(RT_NORMAL_LEVEL)
        } else {
            RT_OFFLINE_LEVEL
        };
        let old_level = level.load(Ordering::Acquire);
        if old_level == new_level {
            return;
        }

        if new_level != RT_OFFLINE_LEVEL {
            self.set_bucket_bit(new_level, cpu);
        }
        // A reader verifies the per-CPU level after observing a bucket bit.
        // Publishing the new membership before this release store means it can
        // never validate a level whose bucket bit is still absent.
        level.store(new_level, Ordering::Release);
        if old_level != RT_OFFLINE_LEVEL {
            self.clear_bucket_bit(old_level, cpu);
        }
    }

    fn find_lower(
        &self,
        waking_priority: u8,
        affinity: &CpuSet,
        preferred: Option<CpuId>,
        accepts: &mut impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        for level in RT_NORMAL_LEVEL..waking_priority {
            if let Some(preferred) = preferred
                && self.contains(level, preferred)
                && affinity.contains(preferred)
                && accepts(preferred)
            {
                return Some(preferred);
            }
            for word_index in 0..self.words_per_level {
                let mut candidates = self.bucket(level, word_index).load(Ordering::Acquire)
                    & affinity.word(word_index);
                while candidates != 0 {
                    let bit = candidates.trailing_zeros() as usize;
                    candidates &= candidates - 1;
                    let index = word_index
                        .saturating_mul(usize::BITS as usize)
                        .saturating_add(bit);
                    let Some(cpu_level) = self.levels.get(index) else {
                        continue;
                    };
                    let cpu = CpuId::new(index as u32);
                    if cpu_level.load(Ordering::Acquire) == level
                        && Some(cpu) != preferred
                        && accepts(cpu)
                    {
                        return Some(cpu);
                    }
                }
            }
        }
        None
    }

    fn contains(&self, level: u8, cpu: CpuId) -> bool {
        self.levels
            .get(cpu.as_usize())
            .is_some_and(|published| published.load(Ordering::Acquire) == level)
            && self
                .bucket(level, cpu.as_usize() / usize::BITS as usize)
                .load(Ordering::Acquire)
                & (1usize << (cpu.as_usize() % usize::BITS as usize))
                != 0
    }

    fn set_bucket_bit(&self, level: u8, cpu: CpuId) {
        self.bucket(level, cpu.as_usize() / usize::BITS as usize)
            .fetch_or(
                1usize << (cpu.as_usize() % usize::BITS as usize),
                Ordering::AcqRel,
            );
    }

    fn clear_bucket_bit(&self, level: u8, cpu: CpuId) {
        self.bucket(level, cpu.as_usize() / usize::BITS as usize)
            .fetch_and(
                !(1usize << (cpu.as_usize() % usize::BITS as usize)),
                Ordering::AcqRel,
            );
    }

    fn bucket(&self, level: u8, word: usize) -> &AtomicUsize {
        &self.buckets[level as usize * self.words_per_level + word]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeadlineCpuEntry {
    cpu: CpuId,
    absolute_deadline_ns: u64,
}

#[derive(Debug)]
struct DeadlineCpuHeap {
    entries: Vec<DeadlineCpuEntry>,
    indices: Vec<Option<usize>>,
    online: CpuSet,
    free: CpuSet,
}

impl DeadlineCpuHeap {
    fn new(cpu_count: usize) -> Self {
        Self {
            entries: Vec::with_capacity(cpu_count),
            indices: vec![None; cpu_count],
            online: CpuSet::empty(cpu_count),
            free: CpuSet::empty(cpu_count),
        }
    }

    fn publish(&mut self, cpu: CpuId, online: bool, earliest_deadline_ns: Option<u64>) {
        if !online {
            self.online.remove(cpu);
            self.free.remove(cpu);
            self.remove(cpu);
            return;
        }
        self.online.insert(cpu);
        match earliest_deadline_ns {
            Some(deadline) => {
                self.free.remove(cpu);
                self.insert_or_update(cpu, deadline);
            }
            None => {
                self.remove(cpu);
                self.free.insert(cpu);
            }
        }
    }

    fn find_later(
        &self,
        absolute_deadline_ns: u64,
        affinity: &CpuSet,
        preferred: Option<CpuId>,
        mut accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        if let Some(preferred) = preferred
            && self.free.contains(preferred)
            && affinity.contains(preferred)
            && accepts(preferred)
        {
            return Some(preferred);
        }
        if let Some(cpu) = self
            .free
            .first_intersection(affinity, |cpu| Some(cpu) != preferred && accepts(cpu))
        {
            return Some(cpu);
        }

        // Match Linux cpudl_find(): once no free allowed CPU exists, only the
        // max-heap root is a valid constant-time candidate. A rejected root
        // leaves select_task_rq semantics on the task's current CPU.
        let entry = self.entries.first()?;
        (crate::scheduler_time_cmp(entry.absolute_deadline_ns, absolute_deadline_ns)
            == core::cmp::Ordering::Greater
            && self.online.contains(entry.cpu)
            && affinity.contains(entry.cpu)
            && accepts(entry.cpu))
        .then_some(entry.cpu)
    }

    fn insert_or_update(&mut self, cpu: CpuId, absolute_deadline_ns: u64) {
        if let Some(index) = self.indices[cpu.as_usize()] {
            let previous = self.entries[index].absolute_deadline_ns;
            self.entries[index].absolute_deadline_ns = absolute_deadline_ns;
            if crate::scheduler_time_cmp(absolute_deadline_ns, previous)
                == core::cmp::Ordering::Greater
            {
                self.sift_up(index);
            } else if crate::scheduler_time_cmp(absolute_deadline_ns, previous)
                == core::cmp::Ordering::Less
            {
                self.sift_down(index);
            }
            return;
        }
        let index = self.entries.len();
        self.entries.push(DeadlineCpuEntry {
            cpu,
            absolute_deadline_ns,
        });
        self.indices[cpu.as_usize()] = Some(index);
        self.sift_up(index);
    }

    fn remove(&mut self, cpu: CpuId) {
        let Some(index) = self.indices[cpu.as_usize()] else {
            return;
        };
        let last = self.entries.len() - 1;
        self.swap(index, last);
        self.entries.pop();
        self.indices[cpu.as_usize()] = None;
        if index < self.entries.len() {
            if index != 0
                && crate::scheduler_time_cmp(
                    self.entries[index].absolute_deadline_ns,
                    self.entries[(index - 1) / 2].absolute_deadline_ns,
                ) == core::cmp::Ordering::Greater
            {
                self.sift_up(index);
            } else {
                self.sift_down(index);
            }
        }
    }

    fn sift_up(&mut self, mut index: usize) {
        while index != 0 {
            let parent = (index - 1) / 2;
            if crate::scheduler_time_cmp(
                self.entries[parent].absolute_deadline_ns,
                self.entries[index].absolute_deadline_ns,
            ) != core::cmp::Ordering::Less
            {
                break;
            }
            self.swap(parent, index);
            index = parent;
        }
    }

    fn sift_down(&mut self, mut index: usize) {
        loop {
            let left = index.saturating_mul(2).saturating_add(1);
            if left >= self.entries.len() {
                break;
            }
            let right = left + 1;
            let largest = if right < self.entries.len()
                && crate::scheduler_time_cmp(
                    self.entries[right].absolute_deadline_ns,
                    self.entries[left].absolute_deadline_ns,
                ) == core::cmp::Ordering::Greater
            {
                right
            } else {
                left
            };
            if crate::scheduler_time_cmp(
                self.entries[index].absolute_deadline_ns,
                self.entries[largest].absolute_deadline_ns,
            ) != core::cmp::Ordering::Less
            {
                break;
            }
            self.swap(index, largest);
            index = largest;
        }
    }

    fn swap(&mut self, left: usize, right: usize) {
        if left == right {
            return;
        }
        self.entries.swap(left, right);
        self.indices[self.entries[left].cpu.as_usize()] = Some(left);
        self.indices[self.entries[right].cpu.as_usize()] = Some(right);
    }
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use core::cell::Cell;

    use super::*;

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn deadline_work_occupies_the_cpupri_higher_bucket() {
        let index = RtCpuPriorityIndex::new(1);
        let cpu = CpuId::new(0);
        let affinity = CpuSet::all(1);
        index.publish(cpu, true, None, true);

        assert_eq!(
            index.find_lower(50, &affinity, Some(cpu), &mut |_| true),
            None,
            "RT placement must not treat runnable Deadline work as normal-priority CPU capacity"
        );

        index.publish(cpu, true, Some(10), false);
        assert_eq!(
            index.find_lower(50, &affinity, Some(cpu), &mut |_| true),
            Some(cpu),
            "removing the last Deadline entity must restore the published RT priority"
        );
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn cpudl_publication_only_locks_for_state_transitions() {
        let index = RootDomainPriorityIndex::new(1);
        let cpu = CpuId::new(0);
        reset_deadline_index_publications();

        index.publish_run_queue(cpu, None, None, true);
        assert_eq!(deadline_index_publications(), 1);
        index.publish_run_queue(cpu, None, None, true);
        assert_eq!(deadline_index_publications(), 1);

        index.publish_run_queue(cpu, None, Some(100), true);
        assert_eq!(deadline_index_publications(), 2);
        index.publish_run_queue(cpu, None, Some(100), true);
        assert_eq!(deadline_index_publications(), 2);
        index.publish_run_queue(cpu, None, Some(200), true);
        assert_eq!(deadline_index_publications(), 3);

        index.publish_offline(cpu);
        assert_eq!(deadline_index_publications(), 4);
        index.publish_offline(cpu);
        assert_eq!(deadline_index_publications(), 4);
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn nonfree_deadline_lookup_reads_only_the_max_heap_root() {
        let mut heap = DeadlineCpuHeap::new(4);
        let affinity = CpuSet::all(4);
        for (cpu, deadline) in [100, 400, 200, 300].into_iter().enumerate() {
            heap.publish(CpuId::new(cpu as u32), true, Some(deadline));
        }
        let accepts_calls = Cell::new(0);

        assert_eq!(
            heap.find_later(50, &affinity, None, |_| {
                accepts_calls.set(accepts_calls.get() + 1);
                true
            }),
            Some(CpuId::new(1))
        );
        assert_eq!(
            accepts_calls.get(),
            1,
            "cpudl must consult its max-heap root instead of scanning every nonfree CPU"
        );
    }
}
