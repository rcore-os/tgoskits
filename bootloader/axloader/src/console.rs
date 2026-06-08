#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        uefi::print!($($arg)*);
    }};
}

#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => {{
        uefi::println!($($arg)*);
    }};
}
