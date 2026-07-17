pub mod memory;
pub mod sync;
pub mod time;

pub use memory::{
    FsPage, FsPageProvider, alloc_page, has_page_provider, install_page_provider, virt_to_phys,
};
pub use time::{BlockTimeProvider, has_time_provider, set_time_provider, wall_time};

/// Installs all OS capabilities used by ax-fs-ng.
pub fn install(
    time_provider: &'static dyn time::BlockTimeProvider,
    page_provider: &'static dyn memory::FsPageProvider,
) {
    time::set_time_provider(time_provider);
    memory::install_page_provider(page_provider);
}
