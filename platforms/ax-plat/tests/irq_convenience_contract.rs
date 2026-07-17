use std::{fs, path::Path};

#[test]
fn shared_timer_convenience_request_owns_one_explicit_enable_transaction() {
    let source = irq_source();
    let generic = function_body(&source, "pub fn request_irq(");
    let shared = function_body(&source, "pub fn request_shared_irq(");

    assert!(
        !generic.contains("registry().enable"),
        "the generic request facade must not enable an auto-enabled registry action twice",
    );
    assert!(shared.contains(".auto_enable(AutoEnable::No)"));
    assert!(shared.contains("request_enabled_irq("));
}

#[test]
fn percpu_ipi_convenience_request_owns_one_explicit_enable_transaction() {
    let source = irq_source();
    let percpu = function_body(&source, "pub fn request_percpu_irq(");
    let transaction = function_body(&source, "fn request_enabled_irq(");

    assert!(percpu.contains(".auto_enable(AutoEnable::No)"));
    assert!(percpu.contains("request_enabled_irq("));
    assert_eq!(transaction.matches("registry().enable(handle)").count(), 1);
    assert!(transaction.contains("registry().free(handle)"));
}

fn irq_source() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join("src/irq.rs")).expect("ax-plat IRQ facade must be readable")
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature `{signature}`"));
    let open = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("function `{signature}` has no body"));
    let mut depth = 0usize;
    for (offset, byte) in source.as_bytes()[open..].iter().copied().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[open + 1..open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("function `{signature}` has an unterminated body")
}
