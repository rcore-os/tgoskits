#![no_std]
#![no_main]
#![doc = include_str!("../../README.md")]

extern crate alloc;

use alloc::{borrow::ToOwned, string::String, vec::Vec};

use ax_std as _;

#[cfg(feature = "nixos")]
pub const DEFAULT_CMDLINE: &[&str] = &["/init"];

#[cfg(all(not(feature = "nixos"), not(feature = "legacy-board-init")))]
pub const DEFAULT_CMDLINE: &[&str] = &["/sbin/init"];

#[cfg(all(not(feature = "nixos"), feature = "legacy-board-init"))]
pub const DEFAULT_CMDLINE: &[&str] = &["/bin/sh", "-c", include_str!("init.sh")];

#[cfg(feature = "nixos")]
const ENVIRON: &[&str] = &["container=starryos"];

#[cfg(not(feature = "nixos"))]
const ENVIRON: &[&str] = &[];

#[unsafe(no_mangle)]
extern "C" fn main() {
    #[cfg(feature = "profile-counter-export")]
    starry_kernel::register_profile_counter_snapshot(snapshot_profile_counters);

    let args = init_command_from_bootargs();
    let envs = ENVIRON
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<Vec<_>>();

    starry_kernel::entry::init(&args, &envs);
}

#[cfg(feature = "profile-counter-export")]
fn snapshot_profile_counters() -> Option<Vec<u8>> {
    use core::sync::atomic::{AtomicU64, Ordering};

    unsafe extern "C" {
        static mut __start___llvm_prf_cnts: u64;
        static mut __stop___llvm_prf_cnts: u64;
    }

    let start = core::ptr::addr_of_mut!(__start___llvm_prf_cnts) as usize;
    let end = core::ptr::addr_of_mut!(__stop___llvm_prf_cnts) as usize;
    let bytes = end.checked_sub(start)?;
    if bytes == 0
        || bytes > 16 * 1024 * 1024
        || !bytes.is_multiple_of(8)
        || !start.is_multiple_of(8)
    {
        return None;
    }

    let mut snapshot = Vec::new();
    snapshot.try_reserve_exact(bytes).ok()?;
    // LLVM updates these counters atomically in the profile-generate build;
    // the linker section stays mapped for the lifetime of the image.
    let counters = start as *const AtomicU64;
    for index in 0..bytes / 8 {
        // SAFETY: the linker bounds and alignment checks cover this counter;
        // the training build requests atomic LLVM updates on every CPU.
        let count = unsafe { (*counters.add(index)).load(Ordering::Relaxed) };
        snapshot.extend_from_slice(&count.to_le_bytes());
    }
    Some(snapshot)
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
    #[cfg(all(not(feature = "nixos"), not(feature = "legacy-board-init")))]
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

#[cfg(all(not(feature = "nixos"), not(feature = "legacy-board-init")))]
const _: () = assert!(command_eq(DEFAULT_CMDLINE, &["/sbin/init"]));

#[cfg(all(not(feature = "nixos"), feature = "legacy-board-init"))]
const _: () = assert!(command_eq(
    DEFAULT_CMDLINE,
    &["/bin/sh", "-c", include_str!("init.sh")]
));

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
