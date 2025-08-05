use alloc::sync::Arc;

use axfs_ng_vfs::Filesystem;
use starry_core::vfs::{DirMaker, DirMapping, SimpleDir, SimpleFs};

pub(crate) fn new_sysfs() -> Filesystem {
    SimpleFs::new_with("sysfs".into(), 0x62656572, builder)
}

fn class_fb0() -> DirMapping {
    // TODO
    DirMapping::new()
}

fn builder(fs: Arc<SimpleFs>) -> DirMaker {
    let mut root = DirMapping::new();
    root.add("class", {
        let mut root = DirMapping::new();
        root.add("graphics", {
            let mut root = DirMapping::new();
            root.add(
                "fb0",
                SimpleDir::new_maker(fs.clone(), Arc::new(class_fb0())),
            );
            SimpleDir::new_maker(fs.clone(), Arc::new(root))
        });
        SimpleDir::new_maker(fs.clone(), Arc::new(root))
    });

    SimpleDir::new_maker(fs, Arc::new(root))
}
