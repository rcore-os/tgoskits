use std::{cell::RefCell, collections::BTreeMap, ffi::CStr, ptr::NonNull};

use anyhow::{Context, bail};

use crate::qemu::ffi;

type RegisterMap = BTreeMap<String, *mut ffi::Register>;

thread_local! {
    // QEMU register handles belong to the vCPU context that enumerated them.
    // A single-threaded TCG executor may host more than one vCPU.
    static REGISTERS: RefCell<BTreeMap<u32, RegisterMap>> = const {
        RefCell::new(BTreeMap::new())
    };
}

/// # Safety
/// Call only from QEMU's vCPU initialization callback on that vCPU's thread.
pub unsafe fn init(cpu: u32) -> anyhow::Result<()> {
    clear(cpu);
    // SAFETY: We are in vCPU init. QEMU returns an owned GArray of complete v7
    // descriptors; their names and handles remain owned by QEMU.
    let array = unsafe { ffi::qemu_plugin_get_registers() };
    let array = NonNull::new(array).context("QEMU returned a null register array")?;
    let result = (|| {
        // SAFETY: The owned GArray is live until g_array_free below.
        let array = unsafe { array.as_ref() };
        let mut registers = BTreeMap::new();
        for index in 0..array.len as usize {
            // SAFETY: QEMU allocated len descriptors with their full v7 stride,
            // including is_readonly. Each index is in bounds and aligned.
            let descriptor = unsafe { &*array.data.cast::<ffi::RegisterDescriptor>().add(index) };
            if descriptor.name.is_null() {
                bail!("QEMU returned a null register name");
            }
            // SAFETY: QEMU's descriptor names are live NUL-terminated strings.
            let name = unsafe { CStr::from_ptr(descriptor.name) }
                .to_str()?
                .to_owned();
            // QEMU encodes a register number in this opaque handle; zero is a
            // valid handle, not a null allocation. It is never dereferenced here.
            registers.insert(name, descriptor.handle);
        }
        REGISTERS.with(|current| current.borrow_mut().insert(cpu, registers));
        Ok(())
    })();
    // SAFETY: The caller owns this array. Names were copied, handles are borrowed
    // from QEMU, and neither points into the array allocation being freed.
    unsafe { ffi::g_array_free(array.as_ptr(), 1) };
    result
}

pub fn clear(cpu: u32) {
    REGISTERS.with(|registers| registers.borrow_mut().remove(&cpu));
}

/// # Safety
/// Call on the initialized vCPU thread inside an execution callback with R_REGS.
pub unsafe fn read(cpu: u32, name: &str) -> anyhow::Result<u64> {
    REGISTERS.with(|registers| {
        let registers = registers.borrow();
        let registers = registers
            .get(&cpu)
            .context("vCPU registers not initialized")?;
        let handle = registers
            .get(name)
            .with_context(|| format!("register {name} not found"))?;
        let output = ByteArray::new()?;
        // SAFETY: The handle belongs to this vCPU and the callback has requested
        // R_REGS. GLib owns the resizable output buffer until its guard is dropped.
        if !unsafe { ffi::qemu_plugin_read_register(*handle, output.0.as_ptr()) } {
            bail!("failed to read register {name}");
        }
        let mut bytes = [0; 8];
        output.copy_to(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    })
}

/// # Safety
/// Call only from QEMU's current-vCPU execution callback, never from the writer.
pub unsafe fn read_memory(addr: u64, output: &mut [u8]) -> anyhow::Result<()> {
    if output.is_empty() {
        return Ok(());
    }
    let buffer = ByteArray::new()?;
    // SAFETY: This is the current vCPU context. QEMU validates guest addresses;
    // the positive length and GLib-owned buffer satisfy its allocation contract.
    if !unsafe { ffi::qemu_plugin_read_memory_vaddr(addr, buffer.0.as_ptr(), output.len()) } {
        bail!("failed to read guest memory at {addr:#x}");
    }
    buffer.copy_to(output)
}

struct ByteArray(NonNull<ffi::GByteArray>);

impl ByteArray {
    fn new() -> anyhow::Result<Self> {
        // SAFETY: GLib initialization is provided by the QEMU process. Ownership
        // of the fresh array transfers to this guard, including on read failure.
        NonNull::new(unsafe { ffi::g_byte_array_new() })
            .map(Self)
            .context("failed to allocate a GLib byte array")
    }

    fn copy_to(&self, output: &mut [u8]) -> anyhow::Result<()> {
        // SAFETY: This guard uniquely owns the live GLib array.
        let array = unsafe { self.0.as_ref() };
        if array.len as usize != output.len() || array.data.is_null() {
            bail!(
                "QEMU returned {} bytes, expected {}",
                array.len,
                output.len()
            );
        }
        // SAFETY: A successful QEMU read initialized the exact checked length.
        // The GLib allocation and caller's slice are disjoint and remain live.
        unsafe { std::ptr::copy_nonoverlapping(array.data, output.as_mut_ptr(), output.len()) };
        Ok(())
    }
}

impl Drop for ByteArray {
    fn drop(&mut self) {
        // SAFETY: The guard owns this array and its segment; no references escape.
        unsafe { ffi::g_byte_array_free(self.0.as_ptr(), 1) };
    }
}
