fn main() {
    autocfg::emit_possibility("borrowedbuf_init");
    autocfg::rerun_path("build.rs");

    let ac = autocfg::new();
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
}
