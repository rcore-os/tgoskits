use std::sync::{Arc, Mutex};

use loongarch_intc_driver::IocsrAccess;
use mmio_api::{MmioAddr, MmioRaw};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IocsrWrite {
    U32 { offset: usize, value: u32 },
    U64 { offset: usize, value: u64 },
}

#[derive(Debug)]
struct FakeIocsrState {
    bytes: Vec<u8>,
    writes: Vec<IocsrWrite>,
}

#[derive(Clone, Debug)]
pub struct FakeIocsr {
    state: Arc<Mutex<FakeIocsrState>>,
}

impl FakeIocsr {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeIocsrState {
                bytes: vec![0; 0x2000],
                writes: Vec::new(),
            })),
        }
    }

    pub fn set_u64(&self, offset: usize, value: u64) {
        let mut state = self.state.lock().unwrap();
        state.bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    pub fn read_u32(&self, offset: usize) -> u32 {
        let state = self.state.lock().unwrap();
        u32::from_le_bytes(state.bytes[offset..offset + 4].try_into().unwrap())
    }

    pub fn read_u64(&self, offset: usize) -> u64 {
        let state = self.state.lock().unwrap();
        u64::from_le_bytes(state.bytes[offset..offset + 8].try_into().unwrap())
    }

    pub fn writes(&self) -> Vec<IocsrWrite> {
        self.state.lock().unwrap().writes.clone()
    }
}

impl IocsrAccess for FakeIocsr {
    fn read_u64(&self, offset: usize) -> u64 {
        self.read_u64(offset)
    }

    fn write_u64(&self, offset: usize, value: u64) {
        let mut state = self.state.lock().unwrap();
        state.bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        state.writes.push(IocsrWrite::U64 { offset, value });
    }

    fn write_u32(&self, offset: usize, value: u32) {
        let mut state = self.state.lock().unwrap();
        state.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        state.writes.push(IocsrWrite::U32 { offset, value });
    }
}

pub fn test_mmio<T>(phys: usize, backing: &mut [T]) -> MmioRaw {
    let size = core::mem::size_of_val(backing);
    let pointer = std::ptr::NonNull::new(backing.as_mut_ptr().cast::<u8>()).unwrap();
    // SAFETY: tests keep `backing` alive and do not resize it while the
    // returned mapping is used; `size` exactly covers the backing slice.
    unsafe { MmioRaw::new(MmioAddr::from(phys), pointer, size) }
}
