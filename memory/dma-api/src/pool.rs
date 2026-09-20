#[cfg(feature = "pool")]
use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
};
use core::ops::{Deref, DerefMut};

#[cfg(feature = "pool")]
use ax_sync::SpinLock as Mutex;

use crate::ContiguousArray;
#[cfg(feature = "pool")]
use crate::{DeviceDma, DmaDirection, DmaError};

#[cfg(feature = "pool")]
#[derive(Clone, Debug)]
pub(crate) struct ContiguousBufferConfig {
    pub size: usize,
    pub align: usize,
    pub direction: DmaDirection,
}

#[cfg(feature = "pool")]
#[derive(Clone)]
pub struct ContiguousBufferPool {
    inner: Arc<Mutex<Inner>>,
}

pub struct ContiguousBuffer {
    data: Option<ContiguousArray<u8>>,
    #[cfg(feature = "pool")]
    pool: Weak<Mutex<Inner>>,
}

unsafe impl Send for ContiguousBuffer {}

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
        #[cfg(feature = "pool")]
        if let Some(data) = self.data.take()
            && let Some(pool) = self.pool.upgrade()
        {
            let mut inner = pool.lock_irqsave();
            inner.dealloc(data);
        }
    }
}

#[cfg(feature = "pool")]
struct Inner {
    dev: DeviceDma,
    config: ContiguousBufferConfig,
    pool: VecDeque<ContiguousArray<u8>>,
}

#[cfg(feature = "pool")]
impl Inner {
    fn alloc(&mut self) -> Option<ContiguousArray<u8>> {
        self.pool.pop_front()
    }

    fn dealloc(&mut self, data: ContiguousArray<u8>) {
        self.pool.push_back(data);
    }
}

#[cfg(feature = "pool")]
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
            inner: Arc::new(Mutex::new(Inner { dev, pool, config })),
        }
    }

    pub fn alloc(&self) -> Result<ContiguousBuffer, DmaError> {
        let config;
        let dev;
        {
            let mut inner = self.inner.lock_irqsave();
            if let Some(data) = inner.alloc() {
                return Ok(ContiguousBuffer {
                    data: Some(data),
                    pool: Arc::downgrade(&self.inner),
                });
            } else {
                config = inner.config.clone();
                dev = inner.dev.clone();
            }
        };

        let data = ContiguousArray::new_zero_with_align(
            &dev,
            config.size,
            config.align,
            config.direction,
        )?;
        Ok(ContiguousBuffer {
            data: Some(data),
            pool: Arc::downgrade(&self.inner),
        })
    }
}
