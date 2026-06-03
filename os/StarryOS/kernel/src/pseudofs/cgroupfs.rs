//! cgroup v2 pseudo-filesystem — `/cgroup/`.

use alloc::{borrow::Cow, boxed::Box, format, string::String, sync::Arc, vec::Vec};

use axfs_ng_vfs::{Filesystem, VfsResult};

use super::{
    DirMaker, NodeOpsMux, RwFile, SimpleDir, SimpleDirOps, SimpleFile, SimpleFileOperation,
    SimpleFs,
};
use crate::cgroup::GLOBAL_CGROUP_ROOT;

const CGROUP2_MAGIC: u32 = 0x63677270;

pub fn new_cgroupfs() -> Filesystem {
    SimpleFs::new_with("cgroup2".into(), CGROUP2_MAGIC, builder)
}

fn builder(fs: Arc<SimpleFs>) -> DirMaker {
    let root = GLOBAL_CGROUP_ROOT.get().expect("cgroup not initialized");
    build_cgroup_dir(fs, root)
}

fn build_cgroup_dir(fs: Arc<SimpleFs>, node: &Arc<crate::cgroup::CgroupNode>) -> DirMaker {
    let ops = CgroupDirOps::new(fs.clone(), node.clone());
    SimpleDir::new_maker(fs, Arc::new(ops))
}

struct CgroupDirOps {
    fs: Arc<SimpleFs>,
    node: Arc<crate::cgroup::CgroupNode>,
}

impl CgroupDirOps {
    fn new(fs: Arc<SimpleFs>, node: Arc<crate::cgroup::CgroupNode>) -> Self {
        Self { fs, node }
    }
}

impl SimpleDirOps for CgroupDirOps {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let static_names = [
            "cgroup.controllers",
            "cgroup.subtree_control",
            "cgroup.type",
            "cgroup.procs",
            "pids.max",
            "pids.current",
            "cpu.weight",
            "cpu.max",
            "cpu.stat",
        ];
        let children = self.node.children.lock();
        let child_names: Vec<String> = children.keys().cloned().collect();
        Box::new(
            static_names
                .into_iter()
                .map(Cow::Borrowed)
                .chain(child_names.into_iter().map(Cow::Owned)),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(match name {
            "cgroup.controllers" => {
                let n = self.node.clone();
                SimpleFile::new_regular(fs, move || Ok(n.controller_list().into_bytes())).into()
            }
            "cgroup.subtree_control" => SimpleFile::new_regular(fs, || Ok(b"".to_vec())).into(),
            "cgroup.type" => SimpleFile::new_regular(fs, || Ok(b"domain\n".to_vec())).into(),
            "cgroup.procs" => {
                let n = self.node.clone();
                SimpleFile::new_regular(
                    fs,
                    RwFile::new(move |req| match req {
                        SimpleFileOperation::Read => {
                            let procs = n.procs.lock();
                            let mut buf = Vec::new();
                            for pid in procs.iter() {
                                buf.extend_from_slice(format!("{}\n", pid).as_bytes());
                            }
                            Ok(Some(buf))
                        }
                        SimpleFileOperation::Write(data) => {
                            let s = core::str::from_utf8(data).unwrap_or("");
                            for line in s.lines() {
                                if let Ok(pid) = line.trim().parse::<u32>() {
                                    let mut procs = n.procs.lock();
                                    if !procs.contains(&pid) {
                                        procs.push(pid);
                                        n.pids.current.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                            }
                            Ok(None)
                        }
                    }),
                )
                .into()
            }
            "pids.max" => {
                let n = self.node.clone();
                SimpleFile::new_regular(
                    fs,
                    RwFile::new(move |req| match req {
                        SimpleFileOperation::Read => {
                            let max = n.pids.max.load(core::sync::atomic::Ordering::Relaxed);
                            if max < 0 {
                                Ok(Some(b"max\n".to_vec()))
                            } else {
                                Ok(Some(format!("{}\n", max).into_bytes()))
                            }
                        }
                        SimpleFileOperation::Write(data) => {
                            let s = core::str::from_utf8(data).unwrap_or("").trim();
                            if s == "max" {
                                n.pids.max.store(-1, core::sync::atomic::Ordering::Relaxed);
                            } else if let Ok(val) = s.parse::<i64>() {
                                n.pids.max.store(val, core::sync::atomic::Ordering::Relaxed);
                            }
                            Ok(None)
                        }
                    }),
                )
                .into()
            }
            "pids.current" => {
                let n = self.node.clone();
                SimpleFile::new_regular(fs, move || {
                    let count = n.pids.current.load(core::sync::atomic::Ordering::Relaxed);
                    Ok(format!("{}\n", count).into_bytes())
                })
                .into()
            }
            "cpu.weight" => {
                let n = self.node.clone();
                SimpleFile::new_regular(
                    fs,
                    RwFile::new(move |req| match req {
                        SimpleFileOperation::Read => {
                            let w = n.cpu.weight.load(core::sync::atomic::Ordering::Relaxed);
                            Ok(Some(format!("{}\n", w).into_bytes()))
                        }
                        SimpleFileOperation::Write(data) => {
                            let s = core::str::from_utf8(data).unwrap_or("").trim();
                            if let Ok(val) = s.parse::<i64>() {
                                let clamped = val.clamp(1, 10000);
                                n.cpu
                                    .weight
                                    .store(clamped, core::sync::atomic::Ordering::Relaxed);
                            }
                            Ok(None)
                        }
                    }),
                )
                .into()
            }
            "cpu.max" => {
                let n = self.node.clone();
                SimpleFile::new_regular(
                    fs,
                    RwFile::new(move |req| match req {
                        SimpleFileOperation::Read => {
                            let quota = n.cpu.cfs_quota.load(core::sync::atomic::Ordering::Relaxed);
                            let period =
                                n.cpu.cfs_period.load(core::sync::atomic::Ordering::Relaxed);
                            if quota < 0 {
                                Ok(Some(format!("max {}\n", period).into_bytes()))
                            } else {
                                Ok(Some(format!("{} {}\n", quota, period).into_bytes()))
                            }
                        }
                        SimpleFileOperation::Write(data) => {
                            let s = core::str::from_utf8(data).unwrap_or("").trim();
                            let parts: Vec<&str> = s.split_whitespace().collect();
                            if !parts.is_empty() {
                                if parts[0] == "max" {
                                    n.cpu
                                        .cfs_quota
                                        .store(-1, core::sync::atomic::Ordering::Relaxed);
                                } else if let Ok(quota) = parts[0].parse::<i64>() {
                                    n.cpu
                                        .cfs_quota
                                        .store(quota, core::sync::atomic::Ordering::Relaxed);
                                }
                            }
                            if parts.len() > 1
                                && let Ok(period) = parts[1].parse::<i64>()
                            {
                                n.cpu
                                    .cfs_period
                                    .store(period, core::sync::atomic::Ordering::Relaxed);
                            }
                            Ok(None)
                        }
                    }),
                )
                .into()
            }
            "cpu.stat" => SimpleFile::new_regular(fs, || {
                Ok(b"nr_periods 0\nnr_throttled 0\nthrottled_usec 0\n".to_vec())
            })
            .into(),
            _ => {
                let children = self.node.children.lock();
                if let Some(child) = children.get(name) {
                    NodeOpsMux::Dir(build_cgroup_dir(fs, child))
                } else {
                    return Err(axfs_ng_vfs::VfsError::NotFound);
                }
            }
        })
    }

    fn is_cacheable(&self) -> bool {
        false
    }

    fn create_dir(&self, name: &str) -> VfsResult<()> {
        self.node.create_child(name)?;
        Ok(())
    }
}
