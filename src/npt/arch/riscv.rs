use page_table_entry::riscv::Rv64PTE;
use page_table_multiarch::{riscv::Sv39MetaData, PageTable64};

use crate::GuestPhysAddr;

pub type NestedPageTable<H> = PageTable64<Sv39MetaData<GuestPhysAddr>, Rv64PTE, H>;
