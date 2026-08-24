// SPDX-License-Identifier: GPL-3.0-or-later OR Apache-2.0

const AARCH64_BACKEND: &str = include_str!("../src/arch/aarch64/mod.rs");

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let signature_start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"));
    let body_start = source[signature_start..]
        .find('{')
        .map(|offset| signature_start + offset)
        .unwrap_or_else(|| panic!("missing function body: {signature}"));

    let mut depth = 0usize;
    for (offset, byte) in source.as_bytes()[body_start..].iter().enumerate() {
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
    panic!("unterminated function body: {signature}");
}

#[test]
fn current_el_irq_closes_idle_before_dispatch() {
    let body = function_body(AARCH64_BACKEND, "fn handle_current_host_irq()");
    let finish_idle = body
        .find("finish_current_idle_wait")
        .expect("AArch64 current-EL IRQ entry must close the host idle interval");
    let acknowledge = body
        .find("acknowledge_host_irq")
        .expect("AArch64 current-EL IRQ entry must acknowledge the host IRQ");

    assert!(
        finish_idle < acknowledge,
        "idle accounting must stop before host IRQ acknowledgement and dispatch"
    );
    assert!(
        body.contains("ax_hal::time::current_ticks()"),
        "idle accounting must use the architectural IRQ-entry counter domain"
    );
}
