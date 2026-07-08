use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

use ax_fs_ng::vfs::FsContext;
use axfs_ng_vfs::DeviceId;

const MS_NOSUID: u32 = 2;
const MS_NODEV: u32 = 4;
const MS_NOEXEC: u32 = 8;
const MS_NOATIME: u32 = 1 << 10;
const MS_RELATIME: u32 = 1 << 21;
const MS_STRICTATIME: u32 = 1 << 24;

pub fn render_mountinfo(fs_context: &FsContext) -> String {
    let entries = fs_context.mount_namespace().walk_tree();
    let mut buf = String::new();
    for (mount_id, parent_id, mp) in entries {
        let root_loc = mp.root_location();

        let mount_point = root_loc
            .absolute_path()
            .map(|p| p.to_string())
            .unwrap_or_else(|_| "/".into());

        let fstype = root_loc.filesystem().name();
        let dev = DeviceId(mp.device());

        let options = render_options(mp.is_readonly(), mp.mount_flags());

        let optional_fields = if mp.is_shared() {
            let gid = mp.peer_group_id();
            if gid != 0 {
                format!("shared:{gid}")
            } else {
                "-".into()
            }
        } else if mp.is_slave() {
            match mp.first_master_peer_group_id() {
                Some(gid) => format!("master:{gid}"),
                None => "-".into(),
            }
        } else if mp.is_unbindable() {
            "unbindable".into()
        } else {
            "-".into()
        };

        let _ = writeln!(
            &mut buf,
            "{mount_id} {parent_id} {}:{} / {mount_point} {options} {optional_fields} - {fstype} \
             {fstype} {options}",
            dev.major(),
            dev.minor(),
        );
    }
    buf
}

pub fn render_mounts(fs_context: &FsContext) -> String {
    let entries = fs_context.mount_namespace().walk_tree();
    let mut buf = String::new();
    for (_, _, mp) in entries {
        let root_loc = mp.root_location();

        let mount_point = root_loc
            .absolute_path()
            .map(|p| p.to_string())
            .unwrap_or_else(|_| "/".into());

        let fstype = root_loc.filesystem().name();
        let options = render_options(mp.is_readonly(), mp.mount_flags());

        let _ = writeln!(&mut buf, "{fstype} {mount_point} {fstype} {options} 0 0",);
    }
    buf
}

fn render_options(readonly: bool, flags: u32) -> String {
    let mut opts: Vec<&str> = Vec::new();
    opts.push(if readonly { "ro" } else { "rw" });
    if flags & MS_NOSUID != 0 {
        opts.push("nosuid");
    }
    if flags & MS_NODEV != 0 {
        opts.push("nodev");
    }
    if flags & MS_NOEXEC != 0 {
        opts.push("noexec");
    }
    if flags & MS_NOATIME != 0 {
        opts.push("noatime");
    }
    if flags & MS_RELATIME != 0 {
        opts.push("relatime");
    }
    if flags & MS_STRICTATIME != 0 {
        opts.push("strictatime");
    }
    opts.join(",")
}

use core::fmt::Write as _;
