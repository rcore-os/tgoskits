#![no_std]
#![no_main]
#![doc = include_str!("../../README.md")]

extern crate alloc;

use alloc::{borrow::ToOwned, string::String, vec::Vec};

use ax_std as _;

#[cfg(feature = "nixos")]
pub const DEFAULT_CMDLINE: &[&str] = &["/init"];

#[cfg(not(feature = "nixos"))]
pub const DEFAULT_CMDLINE: &[&str] = &["/sbin/init"];

#[cfg(feature = "nixos")]
const ENVIRON: &[&str] = &["container=starryos"];

#[cfg(not(feature = "nixos"))]
const ENVIRON: &[&str] = &[];

#[unsafe(no_mangle)]
extern "C" fn main() {
    let args = init_command_from_bootargs();
    let envs = ENVIRON
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<Vec<_>>();

    starry_kernel::entry::init(&args, &envs);
}

fn init_command_from_bootargs() -> Vec<String> {
    let Some(bootargs) = ax_hal::boot::bootargs() else {
        return default_command();
    };

    let mut args = Vec::new();
    for token in bootargs.split_whitespace() {
        if let Some(init) = token.strip_prefix("init=") {
            args.clear();
            args.push(init.to_owned());
        } else if let Some(arg) = token.strip_prefix("initarg=")
            && !args.is_empty()
        {
            args.push(arg.to_owned());
        }
    }

    if args.is_empty() {
        default_command()
    } else {
        args
    }
}

fn default_command() -> Vec<String> {
    #[cfg(not(feature = "nixos"))]
    {
        for path in [
            "/sbin/init",
            "/sbin/openrc",
            "/etc/inittab",
            "/etc/rc.conf",
            "/etc/profile.d/starry.sh",
            "/etc/runlevels/sysinit/starry-runtime",
            "/etc/runlevels/default/starry-autorun",
            "/usr/libexec/starry/console",
        ] {
            let metadata = ax_std::fs::metadata(path).unwrap_or_else(|error| {
                panic!(
                    "Default Alpine boot requires {path}: {error}; prepare the rootfs or use \
                     init=/bin/sh"
                )
            });
            assert!(
                metadata.is_file(),
                "Default Alpine boot requires a file: {path}"
            );
        }
    }
    DEFAULT_CMDLINE
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<Vec<_>>()
}

#[cfg(feature = "nixos")]
const _: () = assert!(command_eq(DEFAULT_CMDLINE, &["/init"]));

#[cfg(not(feature = "nixos"))]
const _: () = assert!(command_eq(DEFAULT_CMDLINE, &["/sbin/init"]));

const fn command_eq(left: &[&str], right: &[&str]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if !bytes_eq(left[index].as_bytes(), right[index].as_bytes()) {
            return false;
        }
        index += 1;
    }
    true
}

const fn bytes_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}
