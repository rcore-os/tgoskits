// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Linux assertion semantics for the source-order DRM translations. In
// particular, GEM_BUG_ON is not erased: a violated source invariant panics.

/// Linux lockdep assertions compile to no-ops in the recorded reference kernel
/// configuration (`CONFIG_LOCKDEP=n`). If this compatibility layer is ever
/// built against a configuration with lockdep enabled, failing loudly is safer
/// than silently claiming that lock ownership was checked.
#[inline]
pub fn lockdep_assert_held<T: ?Sized>(_lock: &T) {
    if crate::config::CONFIG_LOCKDEP {
        panic!("CONFIG_LOCKDEP enabled but LinuxKPI lock ownership tracking is not implemented");
    }
}

#[inline]
pub fn lockdep_assert_not_held<T: ?Sized>(_lock: &T) {
    if crate::config::CONFIG_LOCKDEP {
        panic!("CONFIG_LOCKDEP enabled but LinuxKPI lock ownership tracking is not implemented");
    }
}

#[inline]
pub fn lockdep_assert_held_once<T: ?Sized>(lock: &T) {
    lockdep_assert_held(lock);
}

#[inline]
pub fn lockdep_assert_held_write<T: ?Sized>(lock: &T) {
    lockdep_assert_held(lock);
}

/// Runtime equivalent of Linux `WARN_ON`: evaluate once, report the source
/// location when true, and return the condition to preserve the C macro's
/// expression semantics.
#[track_caller]
pub fn warn_on(condition: bool) -> bool {
    if condition {
        let location = core::panic::Location::caller();
        log::warn!("drm WARN_ON at {}:{}", location.file(), location.line());
    }
    condition
}

/// `missing_case()` reports unsupported switch values without changing
/// control flow, as the Linux helper does.
pub fn missing_case<T: core::fmt::Display>(value: T) {
    let location = core::panic::Location::caller();
    log::warn!(
        "drm missing_case({}) at {}:{}",
        value,
        location.file(),
        location.line()
    );
}

#[macro_export]
macro_rules! BUG_ON {
    ($condition:expr $(,)?) => {{
        let __condition = $condition;
        if __condition {
            panic!(
                "Linux BUG_ON({}) at {}:{}",
                stringify!($condition),
                file!(),
                line!()
            );
        }
    }};
}

#[macro_export]
macro_rules! BUG {
    () => {{ panic!("Linux BUG at {}:{}", file!(), line!()) }};
}

#[macro_export]
macro_rules! WARN_ON {
    ($condition:expr $(,)?) => {{
        let __condition = $condition;
        if __condition {
            log::warn!(
                "drm WARN_ON({}) at {}:{}",
                stringify!($condition),
                file!(),
                line!()
            );
        }
        __condition
    }};
}

#[macro_export]
macro_rules! WARN_ON_ONCE {
    ($condition:expr $(,)?) => {{
        static __WARNED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);
        let __condition = $condition;
        if __condition && !__WARNED.swap(true, core::sync::atomic::Ordering::Relaxed) {
            log::warn!(
                "drm WARN_ON_ONCE({}) at {}:{}",
                stringify!($condition),
                file!(),
                line!()
            );
        }
        __condition
    }};
}

#[macro_export]
macro_rules! WARN_ONCE {
    ($condition:expr, $format:expr $(, $argument:expr)* $(,)?) => {{
        static __WARNED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);
        let __condition = $condition;
        if __condition && !__WARNED.swap(true, core::sync::atomic::Ordering::Relaxed) {
            let __message = alloc::format!($format $(, $argument)*);
            $crate::print::drm_log_at(
                $crate::print::DrmLogLevel::Warn,
                "drm WARN_ONCE",
                file!(),
                line!(),
                &__message,
            );
        }
        __condition
    }};
}

#[macro_export]
macro_rules! drm_WARN_ON {
    ($device:expr, $condition:expr $(,)?) => {{
        let __device = $device;
        let _ = __device;
        let __condition = $condition;
        if __condition {
            log::warn!("drm_WARN_ON at {}:{}", file!(), line!());
        }
        __condition
    }};
}

#[macro_export]
macro_rules! missing_case {
    ($value:expr $(,)?) => {{ $crate::assert::missing_case($value) }};
}

#[macro_export]
macro_rules! lockdep_assert_held {
    ($lock:expr $(,)?) => {{ $crate::assert::lockdep_assert_held(&$lock) }};
}

#[macro_export]
macro_rules! lockdep_assert_not_held {
    ($lock:expr $(,)?) => {{ $crate::assert::lockdep_assert_not_held(&$lock) }};
}
