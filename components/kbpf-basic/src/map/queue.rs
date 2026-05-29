use alloc::vec::Vec;
use core::fmt::Debug;

use super::{BpfMapCommonOps, BpfMapMeta, BpfMapUpdateElemFlags};
use crate::{BpfError, BpfResult as Result};

type BpfQueueValue = Vec<u8>;

/// BPF_MAP_TYPE_QUEUE provides FIFO storage and BPF_MAP_TYPE_STACK provides LIFO storage for BPF programs.
/// These maps support peek, pop and push operations that are exposed to BPF programs through the respective helpers.
/// These operations are exposed to userspace applications using the existing bpf syscall in the following way:
/// - `BPF_MAP_LOOKUP_ELEM` -> `peek`
/// - `BPF_MAP_UPDATE_ELEM` -> `push`
/// - `BPF_MAP_LOOKUP_AND_DELETE_ELEM ` -> `pop`
///
/// See <https://docs.kernel.org/bpf/map_queue_stack.html>
pub trait SpecialMap: Debug + Send + Sync + 'static {
    /// Returns the number of elements the queue can hold.
    fn push(&mut self, value: BpfQueueValue, flags: BpfMapUpdateElemFlags) -> Result<()>;
    /// Removes the first element and returns it.
    fn pop(&mut self) -> Option<BpfQueueValue>;
    /// Returns the first element without removing it.
    fn peek(&self) -> Option<&BpfQueueValue>;
    /// Returns the fixed value size of each entry.
    fn value_size(&self) -> usize;
    /// Get the memory usage of the map.
    fn mem_usage(&self) -> Result<usize>;
}

/// The queue map type is a generic map type, resembling a FIFO (First-In First-Out) queue.
///
/// This map type has no keys, only values. The size and type of the values can be specified by the user
/// to fit a large variety of use cases. The typical use-case for this map type is to keep track of
/// a pool of elements such as available network ports when implementing NAT (network address translation).
///
/// As apposed to most map types, this map type uses a custom set of helpers to pop, peek and push elements.
///
/// See <https://ebpf-docs.dylanreimerink.nl/linux/map-type/BPF_MAP_TYPE_QUEUE/>
#[derive(Debug)]
pub struct QueueMap {
    max_entries: u32,
    value_size: u32,
    data: Vec<BpfQueueValue>,
}

impl QueueMap {
    pub fn new(map_meta: &BpfMapMeta) -> Result<Self> {
        if map_meta.value_size == 0 || map_meta.max_entries == 0 || map_meta.key_size != 0 {
            return Err(BpfError::EINVAL);
        }
        let data = Vec::with_capacity(map_meta.max_entries as usize);
        Ok(Self {
            max_entries: map_meta.max_entries,
            value_size: map_meta.value_size,
            data,
        })
    }
}

impl SpecialMap for QueueMap {
    fn push(&mut self, value: BpfQueueValue, flags: BpfMapUpdateElemFlags) -> Result<()> {
        if flags != BpfMapUpdateElemFlags::empty() {
            return Err(BpfError::EINVAL);
        }
        if self.data.len() == self.max_entries as usize {
            self.data.remove(0);
        }
        self.data.push(value);
        Ok(())
    }

    fn pop(&mut self) -> Option<BpfQueueValue> {
        if self.data.is_empty() {
            return None;
        }
        Some(self.data.remove(0))
    }

    fn peek(&self) -> Option<&BpfQueueValue> {
        self.data.first()
    }

    fn value_size(&self) -> usize {
        self.value_size as usize
    }

    fn mem_usage(&self) -> Result<usize> {
        let mut total = 0;
        for v in &self.data {
            total += v.len();
        }
        Ok(total)
    }
}
/// The stack map type is a generic map type, resembling a stack data structure.
///
/// See <https://ebpf-docs.dylanreimerink.nl/linux/map-type/BPF_MAP_TYPE_STACK/>
#[derive(Debug)]
pub struct StackMap(QueueMap);

impl StackMap {
    /// Create a new [StackMap] with the given key size, value size, and maximum number of entries.
    pub fn new(map_meta: &BpfMapMeta) -> Result<Self> {
        QueueMap::new(map_meta).map(StackMap)
    }
}

impl SpecialMap for StackMap {
    fn push(&mut self, value: BpfQueueValue, flags: BpfMapUpdateElemFlags) -> Result<()> {
        if self.0.data.len() == self.0.max_entries as usize {
            if flags.contains(BpfMapUpdateElemFlags::BPF_EXISTS) {
                // remove the last element
                self.0.data.pop();
            } else {
                return Err(BpfError::ENOMEM);
            }
        }
        self.0.data.push(value);
        Ok(())
    }

    fn pop(&mut self) -> Option<BpfQueueValue> {
        self.0.data.pop()
    }

    fn peek(&self) -> Option<&BpfQueueValue> {
        self.0.data.last()
    }

    fn value_size(&self) -> usize {
        self.0.value_size()
    }

    fn mem_usage(&self) -> Result<usize> {
        self.0.mem_usage()
    }
}

impl<T: SpecialMap> BpfMapCommonOps for T {
    /// Equal to QueueMap::peek
    fn lookup_elem(&mut self, key: &[u8]) -> Result<Option<&[u8]>> {
        if !key.is_empty() {
            return Err(BpfError::EINVAL);
        }
        Ok(self.peek().map(|v| v.as_slice()))
    }

    /// Equal to QueueMap::push
    fn update_elem(&mut self, key: &[u8], value: &[u8], flags: u64) -> Result<()> {
        if !key.is_empty() || value.len() != self.value_size() {
            return Err(BpfError::EINVAL);
        }
        let flag = BpfMapUpdateElemFlags::from_bits(flags).ok_or(BpfError::EINVAL)?;
        if flag.contains(BpfMapUpdateElemFlags::BPF_F_LOCK)
            || (flag.contains(BpfMapUpdateElemFlags::BPF_NOEXIST)
                && flag.contains(BpfMapUpdateElemFlags::BPF_EXISTS))
        {
            return Err(BpfError::EINVAL);
        }
        self.push(value.to_vec(), flag)
    }

    /// Equal to QueueMap::pop
    fn lookup_and_delete_elem(&mut self, key: &[u8], value: &mut [u8]) -> Result<()> {
        if !key.is_empty() || value.len() != self.value_size() {
            return Err(BpfError::EINVAL);
        }
        if let Some(v) = self.pop() {
            value.copy_from_slice(&v);
            Ok(())
        } else {
            Err(BpfError::ENOENT)
        }
    }

    fn push_elem(&mut self, value: &[u8], flags: u64) -> Result<()> {
        self.update_elem(&[], value, flags)
    }

    fn pop_elem(&mut self, value: &mut [u8]) -> Result<()> {
        self.lookup_and_delete_elem(&[], value)
    }

    fn peek_elem(&self, value: &mut [u8]) -> Result<()> {
        if value.len() != self.value_size() {
            return Err(BpfError::EINVAL);
        }
        self.peek()
            .map(|v| value.copy_from_slice(v))
            .ok_or(BpfError::ENOENT)
    }

    fn map_mem_usage(&self) -> Result<usize> {
        self.mem_usage()
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
    use super::QueueMap;
    use crate::{
        BpfError,
        map::{BpfMapCommonOps, BpfMapMeta},
    };

    #[test]
    fn test_queue_validation() {
        let mut meta = BpfMapMeta {
            key_size: 0,
            value_size: 4,
            max_entries: 2,
            ..Default::default()
        };

        let mut queue = QueueMap::new(&meta).unwrap();

        assert_eq!(queue.update_elem(b"x", b"abcd", 0), Err(BpfError::EINVAL));
        assert_eq!(queue.update_elem(&[], b"abc", 0), Err(BpfError::EINVAL));
        assert_eq!(queue.update_elem(&[], b"abcd", 8), Err(BpfError::EINVAL));
        assert_eq!(queue.update_elem(&[], b"abcd", 0), Ok(()));

        let mut value = [0; 3];
        assert_eq!(queue.peek_elem(&mut value), Err(BpfError::EINVAL));
    }
}
