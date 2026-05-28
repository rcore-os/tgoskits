#[cfg(feature = "fs")]
pub use std::fs::{
    File, FileType, create_dir, create_dir_all, metadata, read_dir, read_to_string, remove_dir,
    remove_file, rename,
};
pub use std::io;
pub use std::io::{BufReader, Read};

pub fn stdin() -> io::Stdin {
    std::io::stdin()
}

pub fn stdout() -> io::Stdout {
    std::io::stdout()
}
