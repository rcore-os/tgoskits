use alloc::collections::btree_map::BTreeMap;
use axsync::Mutex;
use lazy_static::lazy_static;

#[derive(Debug, Clone)]
pub struct BiBTreeMap<K, V>
where
    K: Ord + Clone,
    V: Ord + Clone,
{
    pub forward: BTreeMap<K, V>,
    pub reverse: BTreeMap<V, K>,
}

impl<K, V> BiBTreeMap<K, V>
where
    K: Ord + Clone,
    V: Ord + Clone,
{
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        BiBTreeMap {
            forward: BTreeMap::new(),
            reverse: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: K, value: V) {
        if let Some(old_key) = self.reverse.insert(value.clone(), key.clone()) {
            self.forward.remove(&old_key);
        }
        if let Some(old_value) = self.forward.insert(key, value.clone()) {
            self.reverse.remove(&old_value);
        }
    }

    pub fn get_by_key(&self, key: &K) -> Option<&V> {
        self.forward.get(key)
    }

    pub fn get_by_value(&self, value: &V) -> Option<&K> {
        self.reverse.get(value)
    }

    pub fn remove_by_key(&mut self, key: &K) -> Option<V> {
        if let Some(value) = self.forward.remove(key) {
            self.reverse.remove(&value);
            Some(value)
        } else {
            None
        }
    }

    pub fn remove_by_value(&mut self, value: &V) -> Option<K> {
        if let Some(key) = self.reverse.remove(value) {
            self.forward.remove(&key);
            Some(key)
        } else {
            None
        }
    }
}

pub struct IpcidAllocator {
    next_ipcid: i32,
}

impl IpcidAllocator {
    fn new() -> Self {
        IpcidAllocator { next_ipcid: 0 }
    }

    pub fn alloc(&mut self) -> i32 {
        let ipcid = self.next_ipcid;
        self.next_ipcid += 1;
        ipcid
    }
}

lazy_static! {
    pub static ref IPCID_ALLOCATOR: Mutex<IpcidAllocator> = Mutex::new(IpcidAllocator::new());
}
