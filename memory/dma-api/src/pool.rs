use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
};
use core::ops::{Deref, DerefMut};

use ax_kspin::SpinNoIrq as Mutex;

use crate::{ContiguousArray, DeviceDma, DmaDirection, DmaError};

#[derive(Clone, Debug)]
pub(crate) struct ContiguousBufferConfig {
    pub size: usize,
    pub align: usize,
    pub direction: DmaDirection,
}

#[derive(Clone)]
pub struct ContiguousBufferPool {
    inner: Arc<Mutex<Inner>>,
}

pub struct ContiguousBuffer {
    data: Option<ContiguousArray<u8>>,
    pool: Weak<Mutex<Inner>>,
}

impl Deref for ContiguousBuffer {
    type Target = ContiguousArray<u8>;

    fn deref(&self) -> &Self::Target {
        self.data.as_ref().unwrap()
    }
}

impl DerefMut for ContiguousBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data.as_mut().unwrap()
    }
}

impl Drop for ContiguousBuffer {
    fn drop(&mut self) {
        if let Some(data) = self.data.take()
            && let Some(pool) = self.pool.upgrade()
        {
            let mut inner = pool.lock();
            inner.pool.push_back(data);
        }
    }
}

struct Inner {
    pool: VecDeque<ContiguousArray<u8>>,
}

impl ContiguousBufferPool {
    pub(crate) fn with_capacity(
        dev: DeviceDma,
        config: ContiguousBufferConfig,
        cap: usize,
    ) -> ContiguousBufferPool {
        let mut pool = VecDeque::with_capacity(cap);
        for _ in 0..cap {
            if let Ok(data) = ContiguousArray::new_zero_with_align(
                &dev,
                config.size,
                config.align,
                config.direction,
            ) {
                pool.push_back(data);
            }
        }

        ContiguousBufferPool {
            inner: Arc::new(Mutex::new(Inner { pool })),
        }
    }

    /// Takes one preallocated buffer without growing the pool.
    ///
    /// Returns [`DmaError::NoMemory`] immediately when every buffer is in use.
    pub fn alloc(&self) -> Result<ContiguousBuffer, DmaError> {
        let data = self
            .inner
            .lock()
            .pool
            .pop_front()
            .ok_or(DmaError::NoMemory)?;
        Ok(ContiguousBuffer {
            data: Some(data),
            pool: Arc::downgrade(&self.inner),
        })
    }
}
