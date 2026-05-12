use alloc::string::String;

#[derive(Debug)]
pub enum Error {
    Unknown,
    ParseFail(String),
}

pub type Result<T = ()> = core::result::Result<T, Error>;
