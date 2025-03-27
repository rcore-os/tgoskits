#![no_std]

#[macro_use]
extern crate axlog;
extern crate alloc;

mod imp;
mod ptr;

pub use imp::*;

macro_rules! syscall_instrument {(
    $( #[$attr:meta] )*
    $pub:vis
    fn $fname:ident (
        $( $arg_name:ident : $ArgTy:ty ),* $(,)?
    ) -> $RetTy:ty
    $body:block
) => (
    $( #[$attr] )*
    #[allow(unused_parens)]
    $pub
    fn $fname (
        $( $arg_name : $ArgTy ),*
    ) -> $RetTy
    {
        /// Re-emit the original function definition, but as a scoped helper
        $( #[$attr] )*
        fn __original_func__ (
            $($arg_name: $ArgTy),*
        ) -> $RetTy
        $body

        let res = __original_func__($($arg_name),*);
        match res {
            Ok(_) | Err(axerrno::LinuxError::EAGAIN) => debug!(concat!(stringify!($fname), " => {:?}"),  res),
            Err(_) => info!(concat!(stringify!($fname), " => {:?}"), res),
        }
        res
    }
)}
pub(crate) use syscall_instrument;
