#![no_std]
#![no_main]
#![doc = include_str!("../../README.md")]

extern crate alloc;

use alloc::{borrow::ToOwned, string::String, vec, vec::Vec};

use ax_std as _;

const DEFAULT_INITS: &[&str] = &["/sbin/init", "/etc/init", "/bin/init", "/bin/sh"];

#[derive(Default)]
struct InitOptions {
    rdinit: Option<String>,
    init: Option<String>,
    argv: Vec<String>,
    env: Vec<String>,
}

fn init_options(bootargs: &str) -> InitOptions {
    let mut result = InitOptions {
        env: vec!["HOME=/".to_owned(), "TERM=linux".to_owned()],
        ..InitOptions::default()
    };
    #[cfg(feature = "nixos")]
    result.env.push("container=starryos".to_owned());

    let mut after_dashes = false;
    for token in ax_fs_ng::bootargs::tokens(bootargs) {
        if after_dashes {
            result.argv.push(token.to_owned());
        } else if token == "--" {
            after_dashes = true;
        } else if let Some(path) = token.strip_prefix("rdinit=") {
            result.rdinit = Some(path.to_owned());
            result.argv.clear();
        } else if let Some(path) = token.strip_prefix("init=") {
            result.init = Some(path.to_owned());
            result.argv.clear();
        } else if token == "kexec"
            || token.starts_with("BOOT_IMAGE=")
            || token
                .split_once('=')
                .map_or(token.as_str(), |(key, _)| key)
                .contains('.')
            || known_kernel_option(&token)
        {
            continue;
        } else if let Some((name, _)) = token.split_once('=') {
            if let Some(existing) = result
                .env
                .iter_mut()
                .find(|entry| entry.split_once('=').is_some_and(|(key, _)| key == name))
            {
                *existing = token.to_owned();
            } else {
                result.env.push(token.to_owned());
            }
        } else {
            result.argv.push(token.to_owned());
        }
    }
    result
}

// Reserve only this compatibility subset; other Linux options follow the unknown-option path.
fn known_kernel_option(token: &str) -> bool {
    let key = token.split_once('=').map_or(token, |(key, _)| key);
    matches!(
        key,
        "root"
            | "rootfstype"
            | "rootflags"
            | "rootwait"
            | "ro"
            | "rw"
            | "console"
            | "earlycon"
            | "earlyprintk"
            | "keep_bootcon"
            | "loglevel"
            | "quiet"
            | "debug"
            | "initcall_debug"
            | "oops"
            | "panic"
            | "mem"
            | "maxcpus"
            | "nr_cpus"
            | "nosmp"
            | "nokaslr"
    )
}

#[unsafe(no_mangle)]
extern "C" fn main() {
    let options = init_options(ax_hal::boot::bootargs().unwrap_or(""));
    let mut paths = Vec::new();
    if ax_fs_ng::root::root_kind() == Some(ax_fs_ng::root::RootKind::Memory) {
        paths.push(options.rdinit.unwrap_or_else(|| "/init".to_owned()));
    }
    if let Some(init) = options.init {
        paths.push(init);
    } else {
        paths.extend(DEFAULT_INITS.iter().map(|path| (*path).to_owned()));
    }
    starry_kernel::entry::init_candidates(&paths, &options.argv, &options.env);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_parameters_follow_boot_split() {
        let options = init_options(
            "auto root=/dev/sda init=/bin/init nosmp nr_cpus=1 keep_bootcon earlyprintk=serial \
             initcall_debug oops=panic foo=one foo=two -- hi X=1 nosmp",
        );
        assert_eq!(options.init.as_deref(), Some("/bin/init"));
        assert_eq!(options.argv, ["hi", "X=1", "nosmp"]);
        assert!(options.env.contains(&"foo=two".to_owned()));
        assert!(!options.env.contains(&"foo=one".to_owned()));
        assert!(!options.env.iter().any(|entry| entry.starts_with("root=")));
        assert!(
            !options
                .env
                .iter()
                .any(|entry| entry.starts_with("nr_cpus="))
        );
        assert!(
            !options
                .env
                .iter()
                .any(|entry| entry.starts_with("earlyprintk="))
        );
        assert!(!options.env.iter().any(|entry| entry.starts_with("oops=")));
    }

    #[test]
    fn rdinit_resets_previous_arguments() {
        let options = init_options("old rdinit=/early first init=/other second -- third");
        assert_eq!(options.rdinit.as_deref(), Some("/early"));
        assert_eq!(options.init.as_deref(), Some("/other"));
        assert_eq!(options.argv, ["second", "third"]);
    }
}
