mod brk;
mod mincore;
mod mmap;
mod placement;

pub(crate) use mmap::{check_rlimit_as, exec_strings_exceed_rlimit_as};

pub use self::{brk::*, mincore::*, mmap::*};
