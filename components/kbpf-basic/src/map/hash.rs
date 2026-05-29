use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};

use super::{
    BpfCallBackFn, BpfMapCommonOps, BpfMapMeta, BpfMapUpdateElemFlags, PerCpuVariants,
    PerCpuVariantsOps,
};
use crate::{BpfError, BpfResult as Result};
type BpfHashMapKey = Vec<u8>;
type BpfHashMapValue = Vec<u8>;

/// The hash map type is a generic map type with no restrictions on the structure of the key and value.
/// Hash-maps are implemented using a hash table, allowing for lookups with arbitrary keys.
///
/// See <https://ebpf-docs.dylanreimerink.nl/linux/map-type/BPF_MAP_TYPE_HASH/>
#[derive(Debug, Clone)]
pub struct BpfHashMap {
    max_entries: u32,
    key_size: u32,
    value_size: u32,
    data: BTreeMap<BpfHashMapKey, BpfHashMapValue>,
}

impl BpfHashMap {
    /// Create a new [BpfHashMap] with the given key size, value size, and maximum number of entries.
    pub fn new(map_meta: &BpfMapMeta) -> Result<Self> {
        if map_meta.key_size == 0 || map_meta.value_size == 0 || map_meta.max_entries == 0 {
            return Err(BpfError::EINVAL);
        }
        Ok(Self {
            max_entries: map_meta.max_entries,
            key_size: map_meta.key_size,
            value_size: map_meta.value_size,
            data: BTreeMap::new(),
        })
    }

    fn validate_key(&self, key: &[u8]) -> Result<()> {
        if key.len() != self.key_size as usize {
            return Err(BpfError::EINVAL);
        }
        Ok(())
    }

    fn validate_value(&self, value: &[u8]) -> Result<()> {
        if value.len() != self.value_size as usize {
            return Err(BpfError::EINVAL);
        }
        Ok(())
    }
}

impl BpfMapCommonOps for BpfHashMap {
    fn lookup_elem(&mut self, key: &[u8]) -> Result<Option<&[u8]>> {
        self.validate_key(key)?;
        let value = self.data.get(key).map(|v| v.as_slice());
        Ok(value)
    }

    fn update_elem(&mut self, key: &[u8], value: &[u8], flags: u64) -> Result<()> {
        self.validate_key(key)?;
        self.validate_value(value)?;
        let flags = BpfMapUpdateElemFlags::from_bits(flags).ok_or(BpfError::EINVAL)?;
        if flags.contains(BpfMapUpdateElemFlags::BPF_F_LOCK) {
            return Err(BpfError::EINVAL);
        }
        if flags.contains(BpfMapUpdateElemFlags::BPF_NOEXIST)
            && flags.contains(BpfMapUpdateElemFlags::BPF_EXISTS)
        {
            return Err(BpfError::EINVAL);
        }
        let exists = self.data.contains_key(key);
        if flags.contains(BpfMapUpdateElemFlags::BPF_NOEXIST) && exists {
            return Err(BpfError::EEXIST);
        }
        if flags.contains(BpfMapUpdateElemFlags::BPF_EXISTS) && !exists {
            return Err(BpfError::ENOENT);
        }
        if !exists && self.data.len() >= self.max_entries as usize {
            return Err(BpfError::ENOMEM);
        }
        self.data.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete_elem(&mut self, key: &[u8]) -> Result<()> {
        self.validate_key(key)?;
        self.data.remove(key);
        Ok(())
    }

    fn for_each_elem(&mut self, cb: BpfCallBackFn, ctx: *const u8, flags: u64) -> Result<u32> {
        if flags != 0 {
            return Err(BpfError::EINVAL);
        }
        let mut total_used = 0;
        for (key, value) in self.data.iter() {
            let res = cb(key, value, ctx);
            // return value: 0 - continue, 1 - stop and return
            if res != 0 {
                break;
            }
            total_used += 1;
        }
        Ok(total_used)
    }

    fn lookup_and_delete_elem(&mut self, key: &[u8], value: &mut [u8]) -> Result<()> {
        self.validate_key(key)?;
        self.validate_value(value)?;
        let v = self
            .data
            .get(key)
            .map(|v| v.as_slice())
            .ok_or(BpfError::ENOENT)?;
        value.copy_from_slice(v);
        self.data.remove(key);
        Ok(())
    }

    fn get_next_key(&self, key: Option<&[u8]>, next_key: &mut [u8]) -> Result<()> {
        if next_key.len() != self.key_size as usize {
            return Err(BpfError::EINVAL);
        }
        let next = match key {
            None => self.data.iter().next(),
            Some(key) => {
                self.validate_key(key)?;
                if !self.data.contains_key(key) {
                    self.data.iter().next()
                } else {
                    let mut iter = self.data.iter();
                    for (k, _) in iter.by_ref() {
                        if k.as_slice() == key {
                            break;
                        }
                    }
                    iter.next()
                }
            }
        };
        match next {
            Some((k, _)) => {
                next_key.copy_from_slice(k.as_slice());
                Ok(())
            }
            None => Err(BpfError::ENOENT),
        }
    }

    fn map_mem_usage(&self) -> Result<usize> {
        let mut usage = 0;
        for (k, v) in self.data.iter() {
            usage += k.len() + v.len();
        }
        Ok(usage)
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

/// This is the per-CPU variant of the [BpfHashMap] map type.
///
/// See <https://ebpf-docs.dylanreimerink.nl/linux/map-type/BPF_MAP_TYPE_PERCPU_HASH/>
#[derive(Debug)]
pub struct PerCpuHashMap<T: PerCpuVariantsOps> {
    per_cpu_maps: Box<dyn PerCpuVariants<BpfHashMap>>,
    _marker: core::marker::PhantomData<T>,
}

impl<T: PerCpuVariantsOps> PerCpuHashMap<T> {
    /// Create a new [PerCpuHashMap] with the given key size, value size, and maximum number of entries.
    pub fn new(map_meta: &BpfMapMeta) -> Result<Self> {
        let array_map = BpfHashMap::new(map_meta)?;
        let per_cpu_maps = T::create(array_map).ok_or(BpfError::EINVAL)?;
        Ok(PerCpuHashMap {
            per_cpu_maps,
            _marker: core::marker::PhantomData,
        })
    }
}
impl<T: PerCpuVariantsOps> BpfMapCommonOps for PerCpuHashMap<T> {
    fn lookup_elem(&mut self, key: &[u8]) -> Result<Option<&[u8]>> {
        self.per_cpu_maps.get_mut().lookup_elem(key)
    }

    fn update_elem(&mut self, key: &[u8], value: &[u8], flags: u64) -> Result<()> {
        self.per_cpu_maps.get_mut().update_elem(key, value, flags)
    }

    fn delete_elem(&mut self, key: &[u8]) -> Result<()> {
        self.per_cpu_maps.get_mut().delete_elem(key)
    }

    fn for_each_elem(&mut self, cb: BpfCallBackFn, ctx: *const u8, flags: u64) -> Result<u32> {
        self.per_cpu_maps.get_mut().for_each_elem(cb, ctx, flags)
    }

    fn lookup_and_delete_elem(&mut self, key: &[u8], value: &mut [u8]) -> Result<()> {
        self.per_cpu_maps
            .get_mut()
            .lookup_and_delete_elem(key, value)
    }

    fn lookup_percpu_elem(&mut self, key: &[u8], cpu: u32) -> Result<Option<&[u8]>> {
        unsafe { self.per_cpu_maps.force_get_mut(cpu).lookup_elem(key) }
    }

    fn get_next_key(&self, key: Option<&[u8]>, next_key: &mut [u8]) -> Result<()> {
        self.per_cpu_maps.get_mut().get_next_key(key, next_key)
    }

    fn map_mem_usage(&self) -> Result<usize> {
        self.per_cpu_maps.get().map_mem_usage()
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::BpfHashMap;
    use crate::{
        BpfError,
        map::{BpfMapCommonOps, BpfMapMeta, BpfMapUpdateElemFlags},
    };

    #[test]
    fn test_hash_map_flags_and_next_key() {
        let mut meta = BpfMapMeta {
            key_size: 1,
            value_size: 4,
            max_entries: 2,
            ..Default::default()
        };

        let mut map = BpfHashMap::new(&meta).unwrap();

        assert_eq!(
            map.update_elem(b"1", b"aaa1", BpfMapUpdateElemFlags::BPF_NOEXIST.bits()),
            Ok(())
        );
        assert_eq!(
            map.update_elem(b"1", b"aaa2", BpfMapUpdateElemFlags::BPF_NOEXIST.bits()),
            Err(BpfError::EEXIST)
        );
        assert_eq!(
            map.update_elem(b"2", b"bbb2", BpfMapUpdateElemFlags::BPF_EXISTS.bits()),
            Err(BpfError::ENOENT)
        );
        assert_eq!(map.update_elem(b"2", b"bbb2", 0), Ok(()));
        assert_eq!(map.update_elem(b"3", b"ccc3", 0), Err(BpfError::ENOMEM));

        let mut next_key = [0; 1];
        assert_eq!(map.get_next_key(Some(b"9"), &mut next_key), Ok(()));
        assert_eq!(&next_key, b"1");
    }
}
