//! Controller trait definitions and factory registry.
//!
//! Provides the unified registration mechanism for all cgroup v2 controllers.
//! Each controller implements [`CgroupController`] (instance-level attribute I/O)
//! and has a corresponding [`CgroupControllerFactory`] registered at boot.

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use axfs_ng_vfs::VfsResult;

/// Attribute metadata (modeled after Linux `cftype`).
#[derive(Clone, Copy)]
pub struct AttrInfo {
    /// Attribute name without the controller prefix (e.g. `"max"`, not `"pids.max"`).
    pub name: &'static str,
    /// Whether the attribute is read-only.
    pub read_only: bool,
}

/// Per-node controller instance — provides attribute read/write.
///
/// Each [`CgroupNode`](super::CgroupNode) holds one instance per active controller.
pub trait CgroupController: Send + Sync {
    /// Controller name (e.g. `"pids"`, `"cpu"`).
    fn name(&self) -> &str;

    /// Whether this is a domain controller (affects process hosting rules).
    fn is_domain(&self) -> bool {
        false
    }

    /// Read an attribute value at `offset` into `buf`.
    ///
    /// `name` is the attribute suffix (e.g. for `"pids.max"`, `name` = `"max"`).
    fn read_attr(&self, name: &str, offset: usize, buf: &mut [u8]) -> VfsResult<usize>;

    /// Write data to an attribute.
    ///
    /// `name` is the attribute suffix.
    fn write_attr(&self, name: &str, data: &[u8]) -> VfsResult<usize>;

    /// All attributes this controller exposes.
    fn attr_names(&self) -> &[AttrInfo];

    /// Downcast support.
    fn as_any(&self) -> &dyn core::any::Any;
}

/// Factory trait — registered globally, creates controller instances on demand.
pub trait CgroupControllerFactory: Send + Sync {
    /// Controller name.
    fn name(&self) -> &str;

    /// Whether this is a domain controller.
    fn is_domain(&self) -> bool {
        false
    }

    /// Attribute list (used for queries without creating an instance).
    fn attr_names(&self) -> &[AttrInfo];

    /// Create a fresh controller instance for a new cgroup node.
    fn new_instance(&self) -> Arc<dyn CgroupController>;
}

// ── Global factory registry ──────────────────────────────────────────

static FACTORY_REGISTRY: LazyInit<SpinNoIrq<BTreeMap<String, Arc<dyn CgroupControllerFactory>>>> =
    LazyInit::new();

/// Initialize the registry. Called once from [`super::init`].
pub fn init_registry() {
    FACTORY_REGISTRY.init_once(SpinNoIrq::new(BTreeMap::new()));
}

/// Register a controller factory.
pub fn register_factory(factory: Arc<dyn CgroupControllerFactory>) {
    let mut registry = FACTORY_REGISTRY
        .get()
        .expect("controller registry not initialized")
        .lock();
    registry.insert(factory.name().to_string(), factory);
}

/// Look up a factory by controller name.
pub fn get_factory(name: &str) -> Option<Arc<dyn CgroupControllerFactory>> {
    FACTORY_REGISTRY
        .get()
        .expect("controller registry not initialized")
        .lock()
        .get(name)
        .cloned()
}

/// All registered factory names (sorted by BTreeMap key order).
pub fn all_factory_names() -> Vec<String> {
    FACTORY_REGISTRY
        .get()
        .expect("controller registry not initialized")
        .lock()
        .keys()
        .cloned()
        .collect()
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Parse a full attribute name into `(controller, attr_suffix)`.
///
/// `"pids.max"` → `Some(("pids", "max"))`
/// `"cpu.stat"` → `Some(("cpu", "stat"))`
/// `"no_dot"`   → `None`
pub fn parse_attr_name(name: &str) -> Option<(&str, &str)> {
    let dot_pos = name.find('.')?;
    Some((&name[..dot_pos], &name[dot_pos + 1..]))
}

/// Write a string value into `buf` respecting `offset`.
pub fn write_to_buf(value: &str, offset: usize, buf: &mut [u8]) -> VfsResult<usize> {
    let bytes = value.as_bytes();
    if offset >= bytes.len() {
        return Ok(0);
    }
    let n = (bytes.len() - offset).min(buf.len());
    buf[..n].copy_from_slice(&bytes[offset..offset + n]);
    Ok(n)
}
