use std::{env, fs, path::PathBuf};

fn main() {
    autocfg::rerun_path("build.rs");

    let ac = autocfg::new();

    autocfg::emit_possibility("borrowedbuf_init");
    let code = r#"
        #![no_std]
        #![feature(core_io_borrowed_buf)]
        pub fn probe() {
            let _ = core::io::BorrowedBuf::init_len;
        }
    "#;
    if ac.probe_raw(code).is_ok() {
        autocfg::emit("borrowedbuf_init");
    }

    autocfg::emit_possibility("maybe_uninit_slice");
    let code = r#"
        #![no_std]
        pub fn probe() {
            let _ = <[core::mem::MaybeUninit<()>]>::assume_init_mut;
        }
    "#;
    if ac.probe_raw(code).is_ok() {
        autocfg::emit("maybe_uninit_slice");
    }

    let buf_size = env::var("AXIO_DEFAULT_BUF_SIZE")
        .map(|v| v.parse::<usize>().expect("Invalid AXIO_DEFAULT_BUF_SIZE"))
        .unwrap_or(1024 * 2);
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("config.rs"),
        format!(
            "/// Default buffer size for I/O operations.\npub const DEFAULT_BUF_SIZE: usize = {};",
            buf_size
        ),
    )
    .expect("Failed to write config file");
}
