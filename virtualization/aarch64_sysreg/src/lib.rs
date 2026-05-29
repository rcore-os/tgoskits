#![no_std]
#![allow(non_camel_case_types)]

//! # Aarch64 System Register
//!
//! This crate provides a set of types and operations for working with low-level system registers.
//!
//! ## Features
//! - `OperationType`: Defines different types of operations that can be performed.
//! - `RegistersType`: Defines various types of registers.
//! - `SystemRegType`: Defines specific system registers.

mod operation_type;
mod registers_type;
mod system_reg_type;

pub use operation_type::OperationType;
pub use registers_type::RegistersType;
pub use system_reg_type::SystemRegType;
