use page_table_entry::riscv::Rv64PTE;
use page_table_multiarch::{PageTable64, riscv::Sv39MetaData};

use crate::GuestPhysAddr;

pub type NestedPageTable<H> = PageTable64<Sv39MetaData<GuestPhysAddr>, Rv64PTE, H>;
