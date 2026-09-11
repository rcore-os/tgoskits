//! User execution and nofault memory access.

pub use bytemuck::NoUninit;

pub use crate::{
    arch::current::{
        asm::{user_access_ok_page, user_copy},
        uspace::*,
    },
    user_access::{
        UserAccessError, UserAccessType, UserAtomicError, UserAtomicU32Op, user_atomic_u32,
        user_read_u32,
    },
};
