#![no_std]
#![feature(const_option)]
#![feature(const_nonnull_new)]
#![doc = include_str!("../README.md")]

mod pl011;

pub use pl011::Pl011Uart;
