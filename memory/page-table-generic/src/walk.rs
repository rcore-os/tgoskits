use heapless::Vec;

use crate::{PageFrameProvider, PageTableEntry, PageTableRef, TableMeta, VirtAddr, frame::Frame};

/// Maximum stack depth for page table walker
const MAX_WALK_DEPTH: usize = 8;

/// One entry observed by a page-table walk.
#[derive(Debug, Clone, Copy)]
pub struct PteInfo<P: PageTableEntry> {
    /// Entry level, where level one is the leaf table.
    pub level: usize,
    /// Virtual address covered by this entry.
    pub vaddr: VirtAddr,
    /// Whether this entry is a valid leaf or block mapping.
    pub is_final_mapping: bool,
    /// Architecture-specific entry value.
    pub pte: P,
}

/// Half-open virtual range visited by a page-table walker.
#[derive(Debug, Clone, Copy)]
pub struct WalkConfig {
    /// Inclusive start address.
    pub start_vaddr: VirtAddr,
    /// Exclusive end address.
    pub end_vaddr: VirtAddr,
}

/// Allocation-free depth-first page-table walker.
pub struct PageTableWalker<'a, T: TableMeta, A: PageFrameProvider> {
    _phantom: core::marker::PhantomData<&'a ()>,
    config: WalkConfig,
    stack: Vec<WalkState<T, A>, MAX_WALK_DEPTH>,
    finished: bool,
}

#[derive(Clone, Copy)]
struct WalkState<T: TableMeta, A: PageFrameProvider> {
    frame: Frame<T, A>,
    level: usize,
    index: usize,
    base_vaddr: VirtAddr,
}

impl<'a, T: TableMeta, A: PageFrameProvider> PageTableWalker<'a, T, A> {
    /// Creates a walker over `config`.
    ///
    /// # Panics
    ///
    /// Panics when the table geometry exceeds the fixed, allocation-free walk
    /// depth supported by this implementation.
    pub fn new(page_table: &'a PageTableRef<T, A>, config: WalkConfig) -> Self {
        assert!(
            T::LEVEL_BITS.len() <= MAX_WALK_DEPTH,
            "page-table depth exceeds the fixed walker stack"
        );
        let mut walker = Self {
            _phantom: core::marker::PhantomData,
            config,
            stack: Vec::new(),
            finished: false,
        };

        if walker.config.start_vaddr < walker.config.end_vaddr {
            let root_state = WalkState {
                frame: page_table.root.clone(),
                level: Frame::<T, A>::PT_LEVEL,
                index: 0,
                base_vaddr: VirtAddr::from_usize(0),
            };
            assert!(
                walker.stack.push(root_state).is_ok(),
                "the root must fit in the fixed walker stack"
            );
        } else {
            walker.finished = true;
        }

        walker
    }

    fn find_next_entry(&mut self) -> Option<PteInfo<T::P>> {
        loop {
            if self.finished {
                return None;
            }

            if self.stack.is_empty() {
                self.finished = true;
                return None;
            }

            let state = self.stack.last_mut().unwrap();

            if state.index >= state.frame.len() {
                self.stack.pop();
                continue;
            }

            let entries = state.frame.as_slice();
            let pte = entries[state.index];
            state.index += 1;

            let current_vaddr =
                Frame::<T, A>::reconstruct_vaddr(state.index - 1, state.level, state.base_vaddr);

            if current_vaddr < self.config.start_vaddr {
                continue;
            }

            if current_vaddr >= self.config.end_vaddr {
                self.finished = true;
                return None;
            }

            let pte_config = pte.to_config(state.level > 1);
            let is_final_mapping = pte_config.valid && (pte_config.huge || state.level == 1);

            if pte_config.valid && !pte_config.huge && state.level > 1 {
                let child_frame =
                    Frame::from_paddr(pte_config.paddr, state.frame.allocator.clone());

                let child_base_vaddr = current_vaddr;

                let child_state = WalkState {
                    frame: child_frame,
                    level: state.level - 1,
                    index: 0,
                    base_vaddr: child_base_vaddr,
                };

                let level = state.level;
                let vaddr = current_vaddr;

                assert!(
                    self.stack.push(child_state).is_ok(),
                    "table depth must fit in the fixed walker stack"
                );

                return Some(PteInfo {
                    level,
                    vaddr,
                    pte,
                    is_final_mapping,
                });
            }

            return Some(PteInfo {
                level: state.level,
                vaddr: current_vaddr,
                pte,
                is_final_mapping,
            });
        }
    }
}

impl<'a, T: TableMeta, A: PageFrameProvider> Iterator for PageTableWalker<'a, T, A> {
    type Item = PteInfo<T::P>;

    fn next(&mut self) -> Option<Self::Item> {
        self.find_next_entry()
    }
}
