#[test]
fn admitted_idmac_activation_has_no_fallible_recheck_after_doorbell() {
    let service = include_str!("service.rs");
    let activation = service
        .split_once("fn start_idmac_transfer")
        .and_then(|(_, tail)| tail.split_once("fn program_idmac_registers"))
        .map(|(body, _)| body)
        .expect("IDMAC activation must remain an auditable named transition");

    assert!(
        !activation.contains("submit_command_while_registers_owned"),
        "no fallible command-admission check may run after IDMAC owns DMA memory"
    );
}
