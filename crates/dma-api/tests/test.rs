#![cfg(all(test, any(unix, windows)))]

use std::ptr::NonNull;

use dma_api::*;

#[test]
fn test_read() {
    let mut dma: DArray<u32> = new_api()
        .new_array(10, 0x1000, Direction::FromDevice)
        .unwrap();

    dma.set(0, 1);

    let o = dma.read(0).unwrap();

    assert_eq!(o, 1);
}

#[test]
fn test_write() {
    let mut dma: DArray<u32> = new_api()
        .new_array(10, 0x1000, Direction::ToDevice)
        .unwrap();

    dma.set(0, 1);

    let o = dma.read(0).unwrap();

    assert_eq!(o, 1);
}
#[derive(Debug, PartialEq, Eq)]
struct Foo {
    foo: u32,
    bar: u32,
}

#[test]
fn test_modify() {
    let mut dma: DBox<Foo> = new_api().new_box(64, Direction::Bidirectional).unwrap();

    dma.modify(|f| f.bar = 1);

    assert_eq!(dma.read(), Foo { foo: 0, bar: 1 });
}

#[test]
fn test_copy() {
    let mut dma = new_api()
        .new_array::<u32>(0x40, 0x1000, Direction::Bidirectional)
        .unwrap();

    println!("new dma ok");

    let src = [1u32; 0x40];

    dma.copy_from_slice(&src);

    println!("copy ok");

    for (i, &v) in src.iter().enumerate() {
        assert_eq!(dma[i], v);
    }
}

#[test]
fn test_index() {
    let dma = new_api()
        .new_array::<u64>(0x40, 0x1000, Direction::Bidirectional)
        .unwrap();

    println!("new dma ok");

    let a = dma[0];

    assert_eq!(a, 0);
}

fn new_api() -> DeviceDma {
    DeviceDma::new(Impled)
}

struct Impled;

impl DeviceDmaOps for Impled {
    // fn map(&self, addr: std::ptr::NonNull<u8>, size: usize, direction: Direction) -> DmaHandle {
    //     println!("map @{:?}, size {size:#x}, {direction:?}", addr);
    //     addr.as_ptr() as usize as _
    // }

    // fn unmap(&self, addr: std::ptr::NonNull<u8>, size: usize) {
    //     println!("unmap @{:?}, size {size:#x}", addr);
    // }

    fn flush(&self, addr: std::ptr::NonNull<u8>, size: usize) {
        println!("flush @{:?}, size {size:#x}", addr);
    }

    fn invalidate(&self, addr: std::ptr::NonNull<u8>, size: usize) {
        println!("invalidate @{:?}, size {size:#x}", addr);
    }

    fn page_size(&self) -> usize {
        0x1000
    }

    unsafe fn alloc_coherent(&self, layout: core::alloc::Layout) -> Option<DmaHandle> {
        println!(
            "alloc_coherent size: {:#x}, align: {:#x}",
            layout.size(),
            layout.align()
        );
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return None;
        }
        Some(DmaHandle::new(NonNull::new(ptr).unwrap(), ptr as _, layout))
    }

    unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
        println!(
            "dealloc_coherent size: {:#x}, align: {:#x}",
            handle.layout.size(),
            handle.layout.align()
        );
        unsafe { std::alloc::dealloc(handle.dma_addr as usize as _, handle.layout) };
    }
}
