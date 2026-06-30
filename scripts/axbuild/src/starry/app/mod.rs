mod args;
mod board;
mod build_config;
mod discovery;
mod qemu;
mod rootfs;
mod selection;
mod types;

pub use args::{AppCommand, ArgsApp, ArgsAppBoard, ArgsAppList, ArgsAppQemu};
pub(crate) use board::resolve_board_case;
pub(crate) use discovery::discover_apps;
pub(crate) use qemu::{app_qemu_test_case, prepare_qemu_app_case};
pub(crate) use selection::{missing_caps, print_apps, selected_apps};
pub use types::StarryAppKind;
pub(crate) use types::{StarryAppBoardCase, StarryAppCase, StarryAppQemuCase};

#[cfg(test)]
#[path = "tests/support.rs"]
mod test_support;
