#![cfg_attr(not(target_os = "none"), allow(dead_code, unused_imports))]

mod descriptor;
mod irq;
mod manager;
mod tree;

use alloc::{borrow::ToOwned, sync::Arc};

use ax_errno::LinuxResult;
use axfs_ng_vfs::Filesystem;

use self::{irq::manager, manager::UsbFsManager, tree::UsbRootDir};
use crate::pseudofs::{SimpleDir, SimpleFs};

fn create_filesystem(manager: Arc<UsbFsManager>) -> Filesystem {
    info!("usbfs: creating filesystem instance");
    SimpleFs::new_with("usbfs".into(), descriptor::USBFS_MAGIC, move |fs| {
        SimpleDir::new_maker(
            fs.clone(),
            Arc::new(UsbRootDir {
                fs: fs.clone(),
                manager: manager.clone(),
            }),
        )
    })
}

pub(crate) fn new_usbfs() -> LinuxResult<Filesystem> {
    if let Some(manager) = manager() {
        return Ok(create_filesystem(manager));
    }

    info!("usbfs: initializing manager");
    let (hosts, irq_slots) = manager::discover_hosts();
    let manager = Arc::new(UsbFsManager::new(hosts));
    irq::init_globals(manager.clone(), irq_slots);
    let should_spawn_refresh = manager::initialize_hosts(&manager) > 0;

    if should_spawn_refresh {
        info!("usbfs: spawning refresh task");
        let refresh_manager = manager.clone();
        ax_task::spawn_with_name(
            move || ax_task::future::block_on(manager::usbfs_refresh_task(refresh_manager.clone())),
            "usbfs-refresh".to_owned(),
        );
        manager.refresh_event.notify(1);
    }

    Ok(create_filesystem(manager))
}
