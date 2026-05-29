pub(crate) mod ringbuf;
pub use ringbuf::RingBufMap;

use crate::{BpfError, BpfResult as Result, KernelAuxiliaryOps};

#[derive(Debug)]
struct InnerPage<F: KernelAuxiliaryOps> {
    addr: usize,
    _phantom: core::marker::PhantomData<F>,
}

impl<F: KernelAuxiliaryOps> InnerPage<F> {
    pub fn new() -> Result<Self> {
        let addr = F::alloc_page()?;
        Ok(InnerPage {
            addr,
            _phantom: core::marker::PhantomData,
        })
    }

    pub fn phys_addr(&self) -> usize {
        self.addr
    }
}

impl<F: KernelAuxiliaryOps> Drop for InnerPage<F> {
    fn drop(&mut self) {
        F::free_page(self.addr);
    }
}

/// the implementation of the opaque uapi struct bpf_dynptr
#[derive(Debug)]
#[repr(C)]
#[repr(align(8))]
pub struct BpfDynPtr {
    /// Pointer to the data.
    pub data: *mut u8,
    // Size represents the number of usable bytes of dynptr data.
    // If for example the offset is at 4 for a local dynptr whose data is
    // of type u64, the number of usable bytes is 4.
    //
    // The upper 8 bits are reserved. It is as follows:
    // Bits 0 - 23 = size
    // Bits 24 - 30 = dynptr type
    // Bit 31 = whether dynptr is read-only
    pub size: u32,
    /// Offset into the data.
    pub offset: u32,
}

// Since the upper 8 bits of dynptr->size is reserved, the
// maximum supported size is 2^24 - 1.
const DYNPTR_MAX_SIZE: u32 = (1u32 << 24) - 1;
const DYNPTR_TYPE_SHIFT: u32 = 28;
const DYNPTR_SIZE_MASK: u32 = 0xFFFFFF;
const DYNPTR_RDONLY_BIT: u32 = 1 << 31;

impl BpfDynPtr {
    pub fn check_size(size: u32) -> Result<()> {
        if size > DYNPTR_MAX_SIZE {
            return Err(BpfError::EINVAL);
        }
        Ok(())
    }

    pub fn init(&mut self, data: &mut [u8], dynptr_type: BpfDynptrType, offset: u32, size: u32) {
        self.data = data.as_mut_ptr();
        self.offset = offset;
        self.size = size;
        self.size |= (dynptr_type as u32) << DYNPTR_TYPE_SHIFT;
    }
}

#[allow(non_camel_case_types)]
/// the enum bpf_dynptr_type in uapi/linux/bpf.h
pub enum BpfDynptrType {
    BPF_DYNPTR_TYPE_INVALID = 0,
    // Points to memory that is local to the bpf program
    BPF_DYNPTR_TYPE_LOCAL,
    // Underlying data is a ringbuf record
    BPF_DYNPTR_TYPE_RINGBUF,
    // Underlying data is a sk_buff
    BPF_DYNPTR_TYPE_SKB,
    // Underlying data is a xdp_buff
    BPF_DYNPTR_TYPE_XDP,
}
