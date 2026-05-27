/// Object size classes for the slab allocator.
///
/// Each size class corresponds to a fixed object size.
/// Allocations are rounded up to the nearest size class.
use core::alloc::Layout;

/// Number of distinct size classes.
pub const SIZE_CLASS_COUNT: usize = 9;

/// Fixed set of object sizes served by the slab allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SizeClass {
    Bytes8    = 0,
    Bytes16   = 1,
    Bytes32   = 2,
    Bytes64   = 3,
    Bytes128  = 4,
    Bytes256  = 5,
    Bytes512  = 6,
    Bytes1024 = 7,
    Bytes2048 = 8,
}

/// Maximum object size handled by the slab.
pub const SLAB_MAX_SIZE: usize = 2048;

/// Ordered table of (object_size, index) for all classes.
const CLASS_SIZES: [usize; SIZE_CLASS_COUNT] = [8, 16, 32, 64, 128, 256, 512, 1024, 2048];

impl SizeClass {
    /// All size classes in ascending order.
    pub const ALL: [SizeClass; SIZE_CLASS_COUNT] = [
        SizeClass::Bytes8,
        SizeClass::Bytes16,
        SizeClass::Bytes32,
        SizeClass::Bytes64,
        SizeClass::Bytes128,
        SizeClass::Bytes256,
        SizeClass::Bytes512,
        SizeClass::Bytes1024,
        SizeClass::Bytes2048,
    ];

    /// Number of distinct size classes.
    pub const COUNT: usize = SIZE_CLASS_COUNT;

    /// Select the smallest size class that can satisfy `layout`.
    ///
    /// Returns `None` if the requested size or alignment exceeds the slab's capability.
    pub fn from_layout(layout: Layout) -> Option<SizeClass> {
        let size = layout.size().max(layout.align());
        if size > SLAB_MAX_SIZE {
            return None;
        }
        for (i, &class_size) in CLASS_SIZES.iter().enumerate() {
            if size <= class_size {
                return Some(SizeClass::ALL[i]);
            }
        }
        None
    }

    /// Object size in bytes.
    pub const fn size(self) -> usize {
        CLASS_SIZES[self as usize]
    }

    /// Array index (0-based).
    pub const fn index(self) -> usize {
        self as usize
    }

    /// How many pages are needed for a single slab of this class.
    ///
    /// Smaller classes use 1 page, larger classes may use more to amortise the
    /// per-page header overhead.
    pub const fn slab_pages(self, page_size: usize) -> usize {
        let obj_size = self.size();
        if obj_size <= 256 {
            1
        } else if obj_size <= 1024 {
            2
        } else {
            // 2048-byte objects: 4 pages → header + room for objects
            let v = 16 * page_size / (obj_size * 8);
            let v = if v < 4 { v } else { 4 };
            if v < 1 { 1 } else { v }
        }
    }
}
