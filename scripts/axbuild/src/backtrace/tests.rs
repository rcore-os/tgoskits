use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use object::{Object, ObjectSymbol};

use super::{
    BacktraceBlockCapture, BacktraceSymbolizeSession, SymbolizeAfterQemuOutcome,
    apply_qemu_log_retention, flush_pending_stream_symbolize, maybe_symbolize_after_qemu,
    parser::{infer_kind_filter, parse_blocks},
    should_delete_qemu_log_after_symbolize, should_persist_qemu_capture_log, std_test_elf_path,
    symbolize::{
        HostSymbolizer, TextSymbol, is_compiler_local_symbol, write_captured_blocks_to_log,
        write_symbolized_blocks,
    },
    write_raw_blocks_from_output,
};

#[unsafe(no_mangle)]
extern "C" fn bt_symbolize_probe() {
    std::hint::black_box(());
}

#[test]
fn parse_blocks_extracts_frames_with_prefix_noise() {
    let text = r#"
[0.000] INFO something
[0.001] BACKTRACE_BEGIN kind=panic arch=x86_64 alloc=true dwarf=false
[0.001] BT 0 ip=0x1000 fp=0x2000
[0.001] BT 1 ip=0x1010 fp=0x2010
[0.002] BACKTRACE_END
"#;
    let blocks = parse_blocks(text).unwrap();
    assert_eq!(blocks[0].kind, "panic");
    assert_eq!(blocks[0].arch.as_deref(), Some("x86_64"));
    assert_eq!(blocks[0].frames[0].idx, 0);
    assert_eq!(blocks[0].frames[0].ip, 0x1000);
    assert_eq!(blocks[0].frames[0].fp, Some(0x2000));
}

#[test]
fn parse_blocks_captures_bt_error() {
    let text = r#"
BACKTRACE_BEGIN kind=panic arch=aarch64 alloc=false dwarf=false
BT_ERROR requires_alloc
BACKTRACE_END
"#;
    let blocks = parse_blocks(text).unwrap();
    assert_eq!(blocks[0].kind, "panic");
    assert_eq!(blocks[0].errors, vec!["requires_alloc".to_string()]);
    assert!(blocks[0].frames.is_empty());
}

#[test]
fn symbolize_resolves_symbol_with_ip_bias_under_aslr() {
    let exe = std::env::current_exe().unwrap();
    let bytes = std::fs::read(&exe).unwrap();
    let obj = object::File::parse(bytes.as_slice()).unwrap();

    let runtime_ip = bt_symbolize_probe as *const () as usize as u64;
    let mut file_ip = None;
    for sym in obj.symbols() {
        let Ok(name) = sym.name() else {
            continue;
        };
        if name == "bt_symbolize_probe" || name == "_bt_symbolize_probe" {
            file_ip = Some(sym.address());
            break;
        }
    }
    let file_ip = file_ip.expect("failed to find bt_symbolize_probe symbol in current exe");

    let bias = file_ip as i64 - runtime_ip as i64;
    let ip_for_file = runtime_ip.wrapping_add_signed(bias);

    let symbolizer = HostSymbolizer::new(&exe).unwrap();
    let sym = symbolizer.symbolize(ip_for_file).unwrap();
    assert!(sym.contains("bt_symbolize_probe"));
}

#[test]
fn block_capture_writes_only_complete_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("blocks.log");
    let mut capture = BacktraceBlockCapture::create(Some(&log_path), None).unwrap();
    capture
        .push_bytes(
            b"[0.000] noise before\n\
[0.001] BACKTRACE_BEGIN kind=raw arch=x86_64\n\
[0.001] BT 0 ip=0x1000 fp=0x2000\n\
[0.002] BACKTRACE_END\n\
[0.003] more noise\n",
        )
        .unwrap();
    capture.finish().unwrap();

    let text = fs::read_to_string(&log_path).unwrap();
    assert!(!text.contains("noise"));
    assert!(text.contains("BACKTRACE_BEGIN kind=raw"));
    assert!(text.contains("BT 0 ip=0x1000"));
    assert!(text.contains("BACKTRACE_END"));

    let blocks = parse_blocks(&text).unwrap();
    assert_eq!(blocks[0].kind, "raw");
}

#[test]
fn block_capture_tee_suppresses_raw_blocks_on_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("blocks.log");
    let mut capture = BacktraceBlockCapture::create(Some(&log_path), None).unwrap();
    let guest = b"[0.000] boot line\n\
[0.001] BACKTRACE_BEGIN kind=raw arch=x86_64\n\
[0.001] BT 0 ip=0x1000 fp=0x2000\n\
[0.002] BACKTRACE_END\n\
[0.003] after block\n";
    let terminal = capture.push_bytes_for_tee(guest, true).unwrap();
    capture.finish().unwrap();

    let terminal = String::from_utf8(terminal).unwrap();
    assert!(terminal.contains("boot line"));
    assert!(terminal.contains("after block"));
    assert!(!terminal.contains("BACKTRACE_BEGIN"));
    assert!(!terminal.contains("BT 0 ip="));
    assert!(!terminal.contains("BACKTRACE_END"));

    let log = fs::read_to_string(&log_path).unwrap();
    assert!(log.contains("BACKTRACE_BEGIN kind=raw"));
    assert!(log.contains("BT 0 ip=0x1000"));
}

#[test]
fn should_persist_qemu_capture_log_on_keep_or_failure() {
    assert!(should_persist_qemu_capture_log(
        true,
        SymbolizeAfterQemuOutcome::Symbolized,
        true
    ));
    assert!(should_persist_qemu_capture_log(
        false,
        SymbolizeAfterQemuOutcome::Failed,
        true
    ));
    assert!(!should_persist_qemu_capture_log(
        false,
        SymbolizeAfterQemuOutcome::Symbolized,
        true
    ));
    assert!(!should_persist_qemu_capture_log(
        false,
        SymbolizeAfterQemuOutcome::Symbolized,
        false
    ));
}

#[test]
fn apply_qemu_log_retention_removes_file_on_symbolized() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("qemu.log");
    fs::write(&log_path, "BACKTRACE_BEGIN kind=raw\nBACKTRACE_END\n").unwrap();
    apply_qemu_log_retention(&log_path, SymbolizeAfterQemuOutcome::Symbolized, false).unwrap();
    assert!(!log_path.is_file());
}

#[test]
fn maybe_symbolize_after_qemu_keeps_log_when_elf_missing() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("qemu.log");
    let elf_path = dir.path().join("missing.elf");
    fs::write(
        &log_path,
        "BACKTRACE_BEGIN kind=raw arch=x86_64\nBT 0 ip=0x1000\nBACKTRACE_END\n",
    )
    .unwrap();
    let outcome = maybe_symbolize_after_qemu(
        &elf_path,
        &log_path,
        "backtrace-raw-normal",
        false,
        None,
        None,
    )
    .unwrap();
    assert_eq!(outcome, SymbolizeAfterQemuOutcome::Failed);
    assert!(log_path.is_file());
}

#[test]
fn stream_session_symbolizes_on_block_end() {
    let exe = std::env::current_exe().unwrap();
    let session = BacktraceSymbolizeSession::try_new(&exe, "backtrace-raw-normal").unwrap();
    session.on_block_complete(&[
        "[0.001] BACKTRACE_BEGIN kind=raw arch=x86_64".to_string(),
        "[0.001] BT 0 ip=0x1000 fp=0x2000".to_string(),
        "[0.002] BACKTRACE_END".to_string(),
    ]);
    assert!(session.streamed_symbolized());
    assert!(!session.streamed_failed());
}
