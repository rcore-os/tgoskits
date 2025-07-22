#![feature(maybe_uninit_slice)]
#![feature(maybe_uninit_write_slice)]

use std::{
    f32,
    mem::MaybeUninit,
    sync::{LazyLock, Mutex, MutexGuard},
};

use bytemuck::AnyBitPattern;
use extern_trait::extern_trait;
use starry_vm::{VmError, VmIo, VmMutPtr, VmPtr, VmResult, vm_read_slice, vm_write_slice};

static POOL: LazyLock<Mutex<Box<[u8]>>> = LazyLock::new(|| {
    let size = 0x0100_0000; // 1 MiB
    Mutex::new(vec![0; size].into_boxed_slice())
});

struct Vm(MutexGuard<'static, Box<[u8]>>);

#[extern_trait]
unsafe impl VmIo for Vm {
    fn new() -> Self {
        let pool = POOL.lock().unwrap();
        Vm(pool)
    }

    fn read(&mut self, start: usize, buf: &mut [MaybeUninit<u8>]) -> VmResult {
        if start + buf.len() > self.0.len() {
            return Err(VmError::BadAddress);
        }
        let slice = &self.0[start..start + buf.len()];
        buf.write_copy_of_slice(slice);
        Ok(())
    }

    fn write(&mut self, start: usize, buf: &[u8]) -> VmResult {
        if start + buf.len() > self.0.len() {
            return Err(VmError::BadAddress);
        }
        if start < 0x1000 {
            return Err(VmError::AccessDenied);
        }
        let slice = &mut self.0[start..start + buf.len()];
        slice.copy_from_slice(buf);
        Ok(())
    }
}

#[test]
fn test_slice() {
    const DATA: &[u8] = b"Hello, world!";

    let ptr = 0x1000 as *mut u8;
    vm_write_slice(ptr, DATA).unwrap();

    let mut buf = vec![MaybeUninit::uninit(); DATA.len()];
    vm_read_slice(ptr, &mut buf).unwrap();
    let buf = unsafe { buf.assume_init_ref() };
    assert_eq!(buf, DATA);
}

#[test]
fn test_perm() {
    assert_eq!(
        vm_write_slice(0x100 as *mut (), &[]),
        Err(VmError::AccessDenied)
    );
    vm_read_slice(0x200 as *const (), &mut []).unwrap();
}

#[test]
fn test_ptr() {
    #[derive(Debug, Clone, Copy, PartialEq, AnyBitPattern)]
    struct Foo {
        a: i64,
        b: f32,
    }

    const A: Foo = Foo {
        a: 42,
        b: f32::consts::PI,
    };
    const B: Foo = Foo {
        a: 84,
        b: f32::consts::E,
    };
    const C: Foo = Foo {
        a: 168,
        b: f32::consts::TAU,
    };

    let ptr = 0x2000 as *mut Foo;
    vm_write_slice(ptr, &[A, B, C]).unwrap();

    assert_eq!(ptr.vm_read(), Ok(A));
    assert_eq!(ptr.wrapping_add(1).vm_read(), Ok(B));

    let ptr = ptr.wrapping_add(2);
    assert_eq!(ptr.vm_read(), Ok(C));
    ptr.vm_write(A).unwrap();
    assert_eq!(ptr.vm_read(), Ok(A));
}

#[test]
#[cfg(feature = "alloc")]
fn test_load() {
    use starry_vm::vm_load;

    const MAGIC: &[u8] = b"a quick brown fox jumps over the lazy dog";

    let ptr = 0x3000 as *mut u8;
    vm_write_slice(ptr, MAGIC).unwrap();

    assert_eq!(vm_load(ptr, MAGIC.len()).unwrap(), MAGIC);
}

#[test]
#[cfg(feature = "alloc")]
fn test_load_until_nul() {
    use starry_vm::vm_load_until_nul;

    let ptr = 0x4000 as *mut u8;

    assert_eq!(vm_load_until_nul(ptr).unwrap(), []);

    vm_write_slice(ptr, &[b'a', b'b', b'c', 0, b'd', b'e']).unwrap();
    assert_eq!(vm_load_until_nul(ptr).unwrap(), b"abc");

    vm_write_slice(ptr, &[1; 0x1234]).unwrap();
    assert_eq!(vm_load_until_nul(ptr).unwrap().len(), 0x1234);
}
