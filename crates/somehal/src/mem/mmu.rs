use page_table_generic::{PageTable, PagingError};

use crate::mem::ram::Ram;

pub type ArchPageTable<A> = PageTable<<crate::arch::Arch as crate::ArchTrait>::P, A>;

pub type ArchPte =
    <<crate::arch::Arch as crate::ArchTrait>::P as page_table_generic::TableGeneric>::P;

static BOOT_TABLE: spin::Once<ArchPageTable<Ram>> = spin::Once::new();

pub(crate) fn new_boot_table() -> ArchPageTable<Ram> {
    ArchPageTable::<Ram>::new(Ram).unwrap()
}

pub fn new_page_table<A: page_table_generic::FrameAllocator>(
    allocator: A,
) -> Result<ArchPageTable<A>, PagingError> {
    ArchPageTable::<A>::new(allocator)
}

pub(crate) fn set_boot_table(table: ArchPageTable<Ram>) {
    BOOT_TABLE.call_once(|| table);
}
