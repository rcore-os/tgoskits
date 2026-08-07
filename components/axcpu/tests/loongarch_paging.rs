mod paging {
    pub use ax_cpu::paging::MappingFlags;
}

#[path = "../src/loongarch64/paging.rs"]
mod loongarch64_paging;
