use std::{
    f32,
    mem::MaybeUninit,
    sync::{LazyLock, Mutex, MutexGuard},
};

use bytemuck::{AnyBitPattern, NoUninit};
use starry_vm::{VmError, VmIo, VmMutPtr, VmPtr, VmResult, vm_read_slice, vm_write_slice};

static POOL: LazyLock<Mutex<Box<[u8]>>> = LazyLock::new(|| {
    let size = 0x0100_0000; // 1 MiB
    Mutex::new(vec![0; size].into_boxed_slice())
});

struct Vm(MutexGuard<'static, Box<[u8]>>);

unsafe impl VmIo for Vm {
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

fn vm() -> Vm {
    Vm(POOL.lock().unwrap())
}

#[test]
fn test_slice() {
    const DATA: &[u8] = b"Hello, world!";

    let mut vm = vm();
    let ptr = 0x1000 as *mut u8;
    vm_write_slice(&mut vm, ptr, DATA).unwrap();

    let mut buf = vec![MaybeUninit::uninit(); DATA.len()];
    vm_read_slice(&mut vm, ptr, &mut buf).unwrap();
    let buf = unsafe { buf.assume_init_ref() };
    assert_eq!(buf, DATA);
}

#[test]
fn vm_access_requires_an_explicit_provider() {
    let mut vm = Vm(POOL.lock().unwrap());
    let ptr = 0x1800 as *mut u32;

    vm_write_slice(&mut vm, ptr, &[0x1234_5678]).unwrap();
    let mut value = [MaybeUninit::uninit()];
    vm_read_slice(&mut vm, ptr, &mut value).unwrap();

    assert_eq!(unsafe { value[0].assume_init() }, 0x1234_5678);
}

#[test]
fn test_perm() {
    let mut vm = vm();
    assert_eq!(
        vm_write_slice(&mut vm, 0x100 as *mut (), &[]),
        Err(VmError::AccessDenied)
    );
    vm_read_slice(&mut vm, 0x200 as *const (), &mut []).unwrap();
}

#[test]
fn test_ptr() {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, AnyBitPattern, NoUninit)]
    struct Foo {
        a: i64,
        b: f32,
        _padding: u32,
    }

    const A: Foo = Foo {
        a: 42,
        b: f32::consts::PI,
        _padding: 0,
    };
    const B: Foo = Foo {
        a: 84,
        b: f32::consts::E,
        _padding: 0,
    };
    const C: Foo = Foo {
        a: 168,
        b: f32::consts::TAU,
        _padding: 0,
    };

    let mut vm = vm();
    let ptr = 0x2000 as *mut Foo;
    vm_write_slice(&mut vm, ptr, &[A, B, C]).unwrap();

    assert_eq!(ptr.vm_read(&mut vm), Ok(A));
    assert_eq!(ptr.wrapping_add(1).vm_read(&mut vm), Ok(B));

    let ptr = ptr.wrapping_add(2);
    assert_eq!(ptr.vm_read(&mut vm), Ok(C));
    ptr.vm_write(&mut vm, A).unwrap();
    assert_eq!(ptr.vm_read(&mut vm), Ok(A));
}

#[test]
#[cfg(feature = "alloc")]
fn test_load() {
    use starry_vm::vm_load;

    const MAGIC: &[u8] = b"a quick brown fox jumps over the lazy dog";

    let mut vm = vm();
    let ptr = 0x3000 as *mut u8;
    vm_write_slice(&mut vm, ptr, MAGIC).unwrap();

    assert_eq!(vm_load(&mut vm, ptr, MAGIC.len()).unwrap(), MAGIC);
}

#[test]
#[cfg(feature = "alloc")]
fn test_load_until_nul() {
    use starry_vm::vm_load_until_nul;

    let mut vm = vm();
    let ptr = 0x4000 as *mut u8;

    assert_eq!(vm_load_until_nul(&mut vm, ptr).unwrap(), []);

    vm_write_slice(&mut vm, ptr, &[b'a', b'b', b'c', 0, b'd', b'e']).unwrap();
    assert_eq!(vm_load_until_nul(&mut vm, ptr).unwrap(), b"abc");

    vm_write_slice(&mut vm, ptr, &[1; 0x1234]).unwrap();
    assert_eq!(vm_load_until_nul(&mut vm, ptr).unwrap().len(), 0x1234);
}

#[test]
#[cfg(feature = "alloc")]
fn load_until_nul_advances_for_elements_larger_than_one_chunk() {
    use starry_vm::vm_load_until_nul;

    #[repr(transparent)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Wide([u8; 64]);

    let mut vm = vm();
    let ptr = 0x8000 as *mut Wide;
    let nonzero = Wide([1; 64]);
    let zero = Wide([0; 64]);
    vm_write_slice(&mut vm, ptr, &[nonzero, zero]).unwrap();

    assert_eq!(vm_load_until_nul(&mut vm, ptr).unwrap().len(), 1);
}
