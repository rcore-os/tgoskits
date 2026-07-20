use std::{fs, path::PathBuf};

#[test]
fn portable_kpu_core_exposes_no_completion_poll_loop() {
    let source = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .expect("read KPU driver core");

    assert!(!source.contains("pub fn wait_done"));
    assert!(!source.contains("spin_loop"));
    assert!(!source.contains("Error::TimedOut"));
}

#[test]
fn hard_irq_only_captures_completion_and_owner_publishes_it() {
    let kernel = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/StarryOS/kernel/src/pseudofs/dev/kpu.rs"),
    )
    .expect("read Starry KPU adapter");
    let irq = function_body(&kernel, "fn kpu_irq_action(");
    let owner = function_body(&kernel, "fn kpu_maintenance_loop(");

    assert!(!irq.contains("KPU_IRQ_COUNT.fetch_add"));
    assert!(!irq.contains("KPU_DONE_WQ.notify"));
    assert!(owner.contains("KPU_IRQ_COUNT.fetch_add"));
    assert!(owner.contains("KPU_DONE_WQ.notify_all"));
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    let tail = &source[start..];
    let open = tail.find('{').expect("function must have a body");
    let mut depth = 0usize;
    for (offset, byte) in tail[open..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &tail[..open + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function `{signature}`")
}
