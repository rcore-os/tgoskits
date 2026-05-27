use heapless::Vec;

use crate::{RangeError, RangeOp, VecOp};

impl<T: RangeOp + Send + 'static, const N: usize> VecOp<T> for Vec<T, N> {
    fn push(&mut self, item: T) -> Result<(), RangeError<T>> {
        self.push(item).map_err(|_| RangeError::Capacity)
    }

    fn as_slice(&self) -> &[T] {
        self.as_slice()
    }

    fn len(&self) -> usize {
        self.as_slice().len()
    }

    fn remove(&mut self, index: usize) -> T {
        self.remove(index)
    }

    fn insert(&mut self, index: usize, item: T) -> Result<(), RangeError<T>> {
        self.insert(index, item).map_err(|_| RangeError::Capacity)
    }
}
