use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
};
use core::ops::{Deref, DerefMut};

use spin::Mutex;

use crate::{DArray, DeviceDma, DmaDirection, DmaError};

#[derive(Clone, Debug)]
pub(crate) struct DArrayConfig {
    pub size: usize,
    pub align: usize,
    pub direction: DmaDirection,
}

#[derive(Clone)]
pub struct DArrayPool {
    inner: Arc<Mutex<Inner>>,
}

pub struct DBuff {
    data: Option<DArray<u8>>,
    pool: Weak<Mutex<Inner>>,
}

unsafe impl Send for DBuff {}

impl Deref for DBuff {
    type Target = DArray<u8>;

    fn deref(&self) -> &Self::Target {
        self.data.as_ref().unwrap()
    }
}

impl DerefMut for DBuff {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data.as_mut().unwrap()
    }
}

impl Drop for DBuff {
    fn drop(&mut self) {
        if let Some(data) = self.data.take()
            && let Some(pool) = self.pool.upgrade()
        {
            let mut inner = pool.lock();
            inner.dealloc(data);
        }
    }
}

struct Inner {
    dev: DeviceDma,
    config: DArrayConfig,
    pool: VecDeque<DArray<u8>>,
}

impl Inner {
    fn alloc(&mut self) -> Option<DArray<u8>> {
        self.pool.pop_front()
    }

    fn dealloc(&mut self, dvec: DArray<u8>) {
        self.pool.push_back(dvec);
    }
}

impl DArrayPool {
    pub(crate) fn new_pool(dev: DeviceDma, config: DArrayConfig, cap: usize) -> DArrayPool {
        let mut pool = VecDeque::with_capacity(cap);
        for _ in 0..cap {
            if let Ok(dvec) =
                // DArray::zeros(config.dma_mask, config.size, config.align, config.direction)
                DArray::new_zero_with_align(
                    &dev,
                    config.size,
                    config.align,
                    config.direction,
                )
            {
                pool.push_back(dvec);
            }
        }

        DArrayPool {
            inner: Arc::new(Mutex::new(Inner { dev, pool, config })),
        }
    }

    pub fn alloc(&self) -> Result<DBuff, DmaError> {
        let config;
        let dev;
        {
            let mut inner = self.inner.lock();
            if let Some(dvec) = inner.alloc() {
                return Ok(DBuff {
                    data: Some(dvec),
                    pool: Arc::downgrade(&self.inner),
                });
            } else {
                config = inner.config.clone();
                dev = inner.dev.clone();
            }
        };

        let dvec = DArray::new_zero_with_align(&dev, config.size, config.align, config.direction)?;
        Ok(DBuff {
            data: Some(dvec),
            pool: Arc::downgrade(&self.inner),
        })
    }
}
