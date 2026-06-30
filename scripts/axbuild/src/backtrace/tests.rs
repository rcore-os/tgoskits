use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::Parser;
use object::{Object, ObjectSymbol};

use super::{
    BacktraceBlockCapture, BacktraceSymbolizeSession, Command, SymbolizeAfterQemuOutcome,
    apply_qemu_log_retention, arceos_rust_elf_path, flush_pending_stream_symbolize,
    maybe_symbolize_after_qemu,
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
fn infer_kind_filter_from_case_name() {
    assert_eq!(
        infer_kind_filter("backtrace-raw-normal", &[]).as_deref(),
        Some("raw")
    );
    assert_eq!(
        infer_kind_filter("foo-panic-bar", &[]).as_deref(),
        Some("panic")
    );
    assert_eq!(
        infer_kind_filter("my-trap-test", &[]).as_deref(),
        Some("trap")
    );
    assert_eq!(infer_kind_filter("draw-something", &[]), None);
    assert_eq!(infer_kind_filter("fs/shell", &[]), None);
    assert_eq!(infer_kind_filter("ipi", &[]), None);
    let blocks =
        parse_blocks("BACKTRACE_BEGIN kind=panic arch=x86_64\nBT 0 ip=0x1 fp=0x2\nBACKTRACE_END\n")
            .unwrap();
    assert_eq!(
        infer_kind_filter("generic", &blocks).as_deref(),
        Some("panic")
    );
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
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].kind, "panic");
    assert_eq!(blocks[0].arch.as_deref(), Some("x86_64"));
    assert_eq!(blocks[0].frames.len(), 2);
    assert_eq!(blocks[0].frames[0].idx, 0);
    assert_eq!(blocks[0].frames[0].ip, 0x1000);
    assert_eq!(blocks[0].frames[0].fp, Some(0x2000));
}

#[test]
fn parse_blocks_accepts_missing_end_marker() {
    let text = r#"
BACKTRACE_BEGIN kind=trap arch=riscv64
BT 0 ip=0xdead fp=0xbeef
"#;
    let blocks = parse_blocks(text).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].kind, "trap");
    assert_eq!(blocks[0].frames.len(), 1);
}

#[test]
fn parse_blocks_splits_blocks_when_begin_repeats() {
    let text = r#"
BACKTRACE_BEGIN kind=panic arch=x86_64
BT 0 ip=0x1000 fp=0x2000
BACKTRACE_BEGIN kind=trap arch=x86_64
BT 0 ip=0x3000 fp=0x4000
BACKTRACE_END
"#;
    let blocks = parse_blocks(text).unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].kind, "panic");
    assert_eq!(blocks[1].kind, "trap");
}

#[test]
fn parse_blocks_captures_bt_error() {
    let text = r#"
BACKTRACE_BEGIN kind=panic arch=aarch64 alloc=false dwarf=false
BT_ERROR requires_alloc
BACKTRACE_END
"#;
    let blocks = parse_blocks(text).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].kind, "panic");
    assert_eq!(blocks[0].errors, vec!["requires_alloc".to_string()]);
    assert!(blocks[0].frames.is_empty());
}

#[test]
fn parse_blocks_accepts_missing_fp() {
    let text = r#"
BACKTRACE_BEGIN kind=trap arch=riscv64
BT 0 ip=0xdead
BACKTRACE_END
"#;
    let blocks = parse_blocks(text).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].frames.len(), 1);
    assert_eq!(blocks[0].frames[0].ip, 0xdead);
    assert_eq!(blocks[0].frames[0].fp, None);
}

#[test]
fn cli_accepts_adjust_ip_false() {
    #[derive(clap::Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: Command,
    }

    let cli = TestCli::try_parse_from([
        "tg-xtask",
        "symbolize",
        "--elf",
        "/tmp/fake.elf",
        "--adjust-ip",
        "false",
    ])
    .unwrap();

    let Command::Symbolize(args) = cli.command;
    assert!(!args.adjust_ip);
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
fn infer_kind_filter_prefers_raw_from_case_name() {
    let blocks = parse_blocks("BACKTRACE_BEGIN kind=panic\nBT 0 ip=0x1\nBACKTRACE_END\n").unwrap();
    assert_eq!(
        infer_kind_filter("backtrace-raw-normal", &blocks).as_deref(),
        Some("raw")
    );
}

#[test]
fn infer_kind_filter_uses_single_block_kind() {
    let blocks = parse_blocks("BACKTRACE_BEGIN kind=trap\nBT 0 ip=0x1\nBACKTRACE_END\n").unwrap();
    assert_eq!(
        infer_kind_filter("other-case", &blocks).as_deref(),
        Some("trap")
    );
}

#[test]
fn infer_kind_filter_returns_none_for_multiple_kinds() {
    let blocks = parse_blocks(
        r#"
BACKTRACE_BEGIN kind=panic
BT 0 ip=0x1
BACKTRACE_BEGIN kind=trap
BT 0 ip=0x2
BACKTRACE_END
"#,
    )
    .unwrap();
    assert_eq!(infer_kind_filter("mixed", &blocks), None);
}

#[test]
fn arceos_rust_elf_path_uses_release_profile() {
    let path = arceos_rust_elf_path(Path::new("/ws"), "x86_64-unknown-none", "app", false);
    assert_eq!(
        path,
        PathBuf::from("/ws/target/x86_64-unknown-none/release/app")
    );
}

#[test]
fn std_test_elf_path_uses_release_profile() {
    let path = std_test_elf_path(
        Path::new("/ws"),
        "x86_64-unknown-none",
        "arceos-test-suit",
        false,
    );
    assert_eq!(
        path,
        PathBuf::from("/ws/target/x86_64-unknown-linux-musl/release/arceos-test-suit")
    );
}

#[test]
fn std_test_elf_path_maps_arceos_none_target_to_std_target_dir() {
    let path = std_test_elf_path(
        Path::new("/ws"),
        "x86_64-unknown-none",
        "arceos-test-suit",
        false,
    );
    assert_eq!(
        path,
        PathBuf::from("/ws/target/x86_64-unknown-linux-musl/release/arceos-test-suit")
    );
}

#[test]
fn symbolize_skips_zero_ip() {
    let exe = std::env::current_exe().unwrap();
    let symbolizer = HostSymbolizer::new(&exe).unwrap();
    assert!(symbolizer.maybe_symbolize(0).is_none());
}

#[test]
fn write_symbolized_blocks_tolerates_adjustment_exceeding_ip() {
    let exe = std::env::current_exe().unwrap();
    let symbolizer = HostSymbolizer::new(&exe).unwrap();
    let blocks =
        parse_blocks("BACKTRACE_BEGIN kind=raw arch=riscv64\nBT 0 ip=0x1 fp=0x2\nBACKTRACE_END\n")
            .unwrap();

    let mut out = Vec::new();
    write_symbolized_blocks(&mut out, &symbolizer, &blocks, None, true, 0).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("BACKTRACE_BLOCK 0 kind=raw arch=riscv64"));
    assert!(out.contains("BT 0 ip=0x1 fp=0x2"));
}

#[test]
fn compiler_local_symbols_are_not_display_names() {
    assert!(is_compiler_local_symbol(".Lpcrel_hi31487"));
    assert!(is_compiler_local_symbol(".L0"));
    assert!(is_compiler_local_symbol(".Ltmp142"));
    assert!(is_compiler_local_symbol("$x"));
    assert!(is_compiler_local_symbol("$d"));
    assert!(!is_compiler_local_symbol(
        "starry_memtrack_sample_hard_leaf"
    ));
    assert!(!is_compiler_local_symbol(
        "_RNvNtNtNtCs66o47AdWPbf_13starry_kernel8pseudofs3dev8memtrack29record_hard_sample_allocation"
    ));
}

#[test]
fn local_symbol_names_fall_back_to_nearest_text_symbol() {
    let exe = std::env::current_exe().unwrap();
    let mut symbolizer = HostSymbolizer::new(&exe).unwrap();
    symbolizer.text_symbols = vec![
        TextSymbol {
            address: 0x1000,
            size: 0x40,
            name: "starry_memtrack_sample_hard_mid".to_string(),
        },
        TextSymbol {
            address: 0x1040,
            size: 0x80,
            name: "starry_memtrack_sample_hard_leaf".to_string(),
        },
    ];

    assert_eq!(
        symbolizer.display_symbol_name(".Lpcrel_hi31487", 0x1060),
        Some("starry_memtrack_sample_hard_leaf".to_string())
    );
    assert_eq!(
        symbolizer.display_symbol_name(".Lpcrel_hi31487", 0x2000),
        None
    );
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
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].kind, "raw");
    assert_eq!(blocks[0].frames.len(), 1);
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
fn block_capture_tee_forwards_all_bytes_when_not_suppressing() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("blocks.log");
    let mut capture = BacktraceBlockCapture::create(Some(&log_path), None).unwrap();
    let guest = b"BACKTRACE_BEGIN kind=raw\nBT 0 ip=0x1\nBACKTRACE_END\n";
    let terminal = capture.push_bytes_for_tee(guest, false).unwrap();
    assert_eq!(terminal, guest);
}

#[test]
fn block_capture_splits_on_repeated_begin() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("blocks.log");
    let mut capture = BacktraceBlockCapture::create(Some(&log_path), None).unwrap();
    capture
        .push_bytes(
            b"BACKTRACE_BEGIN kind=panic arch=x86_64\n\
BT 0 ip=0x1 fp=0x2\n\
BACKTRACE_BEGIN kind=trap arch=x86_64\n\
BT 0 ip=0x3 fp=0x4\n\
BACKTRACE_END\n",
        )
        .unwrap();
    capture.finish().unwrap();

    let text = fs::read_to_string(&log_path).unwrap();
    let blocks = parse_blocks(&text).unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].kind, "panic");
    assert_eq!(blocks[1].kind, "trap");
}

#[test]
fn block_capture_accepts_bt_error_block() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("blocks.log");
    let mut capture = BacktraceBlockCapture::create(Some(&log_path), None).unwrap();
    capture
        .push_bytes(
            b"BACKTRACE_BEGIN kind=panic arch=aarch64\n\
BT_ERROR requires_alloc\n\
BACKTRACE_END\n",
        )
        .unwrap();
    capture.finish().unwrap();

    let text = fs::read_to_string(&log_path).unwrap();
    let blocks = parse_blocks(&text).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].errors, vec!["requires_alloc".to_string()]);
}

#[test]
fn write_raw_blocks_from_output_filters_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("filtered.log");
    let transcript = "boot log\nBACKTRACE_BEGIN kind=raw\nBT 0 ip=0x10\nBACKTRACE_END\n";
    write_raw_blocks_from_output(transcript, &log_path).unwrap();
    let text = fs::read_to_string(&log_path).unwrap();
    assert!(!text.contains("boot log"));
    assert!(text.contains("BACKTRACE_BEGIN"));
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
fn block_capture_memory_only_skips_log_file() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("blocks.log");
    let pending = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut capture = BacktraceBlockCapture::create(None, Some(pending.clone())).unwrap();
    capture
        .push_bytes(
            b"BACKTRACE_BEGIN kind=raw arch=x86_64\n\
BT 0 ip=0x1000 fp=0x2000\n\
BACKTRACE_END\n",
        )
        .unwrap();
    capture.finish().unwrap();
    assert!(!log_path.is_file());
    let blocks = pending.lock().unwrap();
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0][0].contains("BACKTRACE_BEGIN"));
}

#[test]
fn write_captured_blocks_to_log_writes_raw_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("qemu.log");
    let blocks = vec![vec![
        "BACKTRACE_BEGIN kind=raw".to_string(),
        "BT 0 ip=0x10".to_string(),
        "BACKTRACE_END".to_string(),
    ]];
    write_captured_blocks_to_log(&log_path, &blocks).unwrap();
    let text = fs::read_to_string(&log_path).unwrap();
    assert!(text.contains("BACKTRACE_BEGIN"));
    assert!(!text.contains("boot"));
}

#[test]
fn should_delete_qemu_log_only_after_success_without_keep() {
    assert!(should_delete_qemu_log_after_symbolize(
        SymbolizeAfterQemuOutcome::Symbolized,
        false
    ));
    assert!(!should_delete_qemu_log_after_symbolize(
        SymbolizeAfterQemuOutcome::Symbolized,
        true
    ));
    assert!(!should_delete_qemu_log_after_symbolize(
        SymbolizeAfterQemuOutcome::Failed,
        false
    ));
    assert!(!should_delete_qemu_log_after_symbolize(
        SymbolizeAfterQemuOutcome::Skipped,
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
fn apply_qemu_log_retention_keeps_file_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("qemu.log");
    fs::write(&log_path, "BACKTRACE_BEGIN kind=raw\nBACKTRACE_END\n").unwrap();
    apply_qemu_log_retention(&log_path, SymbolizeAfterQemuOutcome::Symbolized, true).unwrap();
    assert!(log_path.is_file());
}

#[test]
fn apply_qemu_log_retention_keeps_file_on_failed_symbolize() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("qemu.log");
    fs::write(&log_path, "truncated BACKTRACE_BEGIN\n").unwrap();
    apply_qemu_log_retention(&log_path, SymbolizeAfterQemuOutcome::Failed, false).unwrap();
    assert!(log_path.is_file());
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

#[test]
fn maybe_symbolize_after_qemu_skips_reread_when_stream_ok() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("qemu.log");
    let exe = std::env::current_exe().unwrap();
    let memory_blocks = vec![vec![
        "BACKTRACE_BEGIN kind=raw arch=x86_64".to_string(),
        "BT 0 ip=0x1000".to_string(),
        "BACKTRACE_END".to_string(),
    ]];
    let session = BacktraceSymbolizeSession::try_new(&exe, "backtrace-raw-normal").unwrap();
    session.on_block_complete(&memory_blocks[0]);
    let outcome = maybe_symbolize_after_qemu(
        &exe,
        &log_path,
        "backtrace-raw-normal",
        false,
        Some(&session),
        Some(&memory_blocks),
    )
    .unwrap();
    assert_eq!(outcome, SymbolizeAfterQemuOutcome::Symbolized);
    assert!(!log_path.is_file());
}

#[test]
fn maybe_symbolize_after_qemu_writes_log_when_keep_requested() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("qemu.log");
    let exe = std::env::current_exe().unwrap();
    let memory_blocks = vec![vec![
        "BACKTRACE_BEGIN kind=raw arch=x86_64".to_string(),
        "BT 0 ip=0x1000".to_string(),
        "BACKTRACE_END".to_string(),
    ]];
    let session = BacktraceSymbolizeSession::try_new(&exe, "backtrace-raw-normal").unwrap();
    session.on_block_complete(&memory_blocks[0]);
    let outcome = maybe_symbolize_after_qemu(
        &exe,
        &log_path,
        "backtrace-raw-normal",
        true,
        Some(&session),
        Some(&memory_blocks),
    )
    .unwrap();
    assert_eq!(outcome, SymbolizeAfterQemuOutcome::Symbolized);
    assert!(log_path.is_file());
    let text = fs::read_to_string(&log_path).unwrap();
    assert!(text.contains("BACKTRACE_BEGIN"));
}

#[test]
fn block_capture_queues_stream_blocks_for_symbolize() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("blocks.log");
    let exe = std::env::current_exe().unwrap();
    let session = BacktraceSymbolizeSession::try_new(&exe, "backtrace-raw-normal").unwrap();
    let pending = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut capture =
        BacktraceBlockCapture::create(Some(&log_path), Some(pending.clone())).unwrap();
    capture
        .push_bytes(
            b"BACKTRACE_BEGIN kind=raw arch=x86_64\n\
BT 0 ip=0x1000 fp=0x2000\n\
BACKTRACE_END\n",
        )
        .unwrap();
    capture.finish().unwrap();
    flush_pending_stream_symbolize(&session, &pending);
    assert!(session.streamed_symbolized());
}
