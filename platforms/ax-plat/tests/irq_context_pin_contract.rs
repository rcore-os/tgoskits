//! IRQ-context lookup must keep the CPU identity and context-bit read in one pin window.

const IRQ_SOURCE: &str = include_str!("../src/irq.rs");

#[test]
fn irq_context_lookup_keeps_cpu_identity_pinned_through_the_bit_read() {
    let body = function_body(IRQ_SOURCE, "pub fn in_irq_context() -> bool");

    assert!(
        body.contains("NoPreempt::new()"),
        "IRQ-context lookup must prevent migration across the complete snapshot"
    );
    assert!(
        body.contains("with_cpu_pin"),
        "IRQ-context lookup must resolve the CPU through an explicit pin"
    );
    assert!(
        body.contains("this_cpu_id_pinned"),
        "IRQ-context lookup must not release an inner guard between ID and bit reads"
    );
    assert!(
        !body.contains("PlatIrqOps.current_cpu()"),
        "a CPU ID returned after an inner preemption guard is released can be stale"
    );
}

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"));
    let body_start = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .expect("function must have a body");
    let mut depth = 0usize;

    for (offset, byte) in source[body_start..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[body_start..=body_start + offset];
                }
            }
            _ => {}
        }
    }

    panic!("function body is not balanced: {signature}");
}
