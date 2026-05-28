use crate::hal::fs::io;
use std::string::String;

pub fn current_dir() -> io::Result<String> {
    std::env::current_dir()
}

pub fn set_current_dir(path: &str) -> io::Result<()> {
    std::env::set_current_dir(path)
}
