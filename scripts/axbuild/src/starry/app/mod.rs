mod args;
mod board;
mod board_assets;
mod build_config;
mod discovery;
mod linux_stage;
mod qemu;
mod rootfs;
mod selection;
mod types;

pub use args::{AppCommand, ArgsApp, ArgsAppBoard, ArgsAppList, ArgsAppQemu};
pub(crate) use board::{merge_board_init_command, resolve_board_case};
pub(in crate::starry) use board_assets::prepare_app_board_session_assets;
pub(crate) use discovery::discover_apps;
pub(in crate::starry) use linux_stage::stage_in_default_linux;
pub(crate) use qemu::{app_qemu_test_case, prepare_qemu_app_case};
pub(crate) use selection::{missing_caps, print_apps, selected_apps};
pub use types::StarryAppKind;
pub(crate) use types::{StarryAppBoardCase, StarryAppCase, StarryAppQemuCase};

#[cfg(test)]
#[path = "tests/support.rs"]
mod test_support;
