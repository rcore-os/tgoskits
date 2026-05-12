use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ops::Deref,
    sync::atomic::{AtomicBool, Ordering},
};

pub struct StaticCell<T> {
    init: AtomicBool,
    value: UnsafeCell<MaybeUninit<T>>,
}

unsafe impl<T: Send> Sync for StaticCell<T> {}
unsafe impl<T: Send> Send for StaticCell<T> {}

impl<T> StaticCell<T> {
    pub const fn uninit() -> Self {
        StaticCell {
            init: AtomicBool::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    pub const fn new(val: T) -> Self {
        StaticCell {
            init: AtomicBool::new(true),
            value: UnsafeCell::new(MaybeUninit::new(val)),
        }
    }

    pub fn init(&self, val: T) {
        if self
            .init
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            panic!(
                "LazyStatic {} already initialized",
                core::any::type_name::<T>()
            );
        }
        unsafe { (*self.value.get()).as_mut_ptr().write(val) };
    }

    pub fn is_init(&self) -> bool {
        self.init.load(Ordering::Acquire)
    }

    /// 初始化单核场景下的值
    /// # Safety
    /// thread-unsafe
    pub unsafe fn init_single_core(&self, val: T) {
        if self.init.load(Ordering::Relaxed) {
            panic!(
                "LazyStatic {} already initialized",
                core::any::type_name::<T>()
            );
        }
        unsafe { (*self.value.get()).as_mut_ptr().write(val) };
        self.init.store(true, Ordering::Relaxed);
    }

    /// 更新已初始化的值
    /// # Safety
    /// 调用者必须确保该值已经初始化
    /// thread-unsafe
    pub unsafe fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        if !self.init.load(Ordering::Acquire) {
            panic!("LazyStatic {} not initialized", core::any::type_name::<T>());
        }
        let val = unsafe { &mut *(*self.value.get()).as_mut_ptr() };
        f(val)
    }
}

impl<T> Deref for StaticCell<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        if !self.init.load(Ordering::Acquire) {
            panic!("LazyStatic {} not initialized", core::any::type_name::<T>());
        }
        unsafe { &*(*self.value.get()).as_ptr() }
    }
}
