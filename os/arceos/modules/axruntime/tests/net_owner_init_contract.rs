use std::{fs, path::PathBuf};

fn source() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .and_then(|path| path.parent())
        .and_then(|path| path.parent())
        .expect("ax-runtime must remain under os/arceos/modules")
        .to_path_buf();
    fs::read_to_string(root.join("os/arceos/modules/axruntime/src/net.rs"))
        .expect("net owner source must be readable")
}

#[test]
fn discovered_device_is_not_forced_through_ready_only_irq_control() {
    let source = source();
    let owner = source
        .split("fn run_net_owner(")
        .nth(1)
        .expect("run_net_owner must exist");
    let register = owner
        .find("prepare_net_irq_owner")
        .expect("owner must register its IRQ endpoint");
    let action_enable = owner
        .find("registration.enable()")
        .expect("owner must enable its disabled action");
    let initialize = owner
        .find("drive_net_owner_init")
        .expect("owner must drive initialization on the pinned thread");

    assert!(
        register < action_enable && action_enable < initialize,
        "the disabled action must exist and become live before owner initialization"
    );
    assert!(
        !owner[..initialize].contains("net.disable_irq()"),
        "discovered devices must not be passed to ready-only queue IRQ control"
    );
}

#[test]
fn os_action_is_live_before_the_first_driver_init_transition() {
    let source = source();
    let owner = source
        .split_once("fn run_net_owner")
        .expect("net owner entry must exist")
        .1;
    let enable = owner
        .find("registration.enable()")
        .expect("owner must enable its registered OS action");
    let initialize = owner
        .find("drive_net_owner_init")
        .expect("owner must drive the portable init state machine");
    assert!(
        enable < initialize,
        "the OS action must be enabled before the first hardware init transition"
    );
}

#[test]
fn ready_queue_activation_is_contained_by_the_os_action() {
    let source = source();
    let owner = source
        .split_once("fn run_net_owner")
        .expect("net owner entry must exist")
        .1;
    let initialize = owner
        .find("drive_net_owner_init")
        .expect("owner must finish initialization first");
    let action_disable = owner[initialize..]
        .find("registration.disable()")
        .map(|offset| initialize + offset)
        .expect("queue activation must disable its OS action");
    let synchronize = owner[action_disable..]
        .find("registration.synchronize()")
        .map(|offset| action_disable + offset)
        .expect("queue activation must drain in-flight callbacks");
    let device_disable = owner[synchronize..]
        .find("net.disable_irq()")
        .map(|offset| synchronize + offset)
        .expect("ready device queue sources must be suppressed");
    let activate = owner
        .find("net.activate_queues()")
        .expect("owner must activate queues");

    assert!(
        initialize < action_disable
            && action_disable < synchronize
            && synchronize < device_disable
            && device_disable < activate,
        "queue DMA publication must remain behind a disabled and synchronized OS action"
    );
}

#[test]
fn captured_init_event_is_consumed_before_its_source_is_rearmed() {
    let source = source();
    let init = source
        .split_once("fn drive_net_owner_init")
        .expect("net init driver must exist")
        .1;
    let consume = init
        .find("OwnerInitInput::with_event")
        .expect("captured event must be passed to the portable state machine");
    let rearm = init
        .find("net.rearm_irq_source(source)")
        .expect("masked source must be rearmed after event consumption");
    assert!(
        consume < rearm,
        "source rearm before portable event consumption can lose a level assertion"
    );
    assert!(
        init.contains("session.drain_owner(1"),
        "init must leave events after the Ready transition in the mailbox for normal service"
    );
}

#[test]
fn initialization_has_no_implicit_retry_or_device_completion_probe() {
    let source = source();
    let init = source
        .split_once("fn drive_net_owner_init")
        .expect("net init driver must exist")
        .1;
    for forbidden in ["sleep(", "poll_completions", "poll_request", "block_until"] {
        assert!(
            !init.contains(forbidden),
            "owner init must wait only for declared IRQ/deadline activation: {forbidden}"
        );
    }
}

#[test]
fn normal_io_rearms_masked_sources_only_after_irq_obligations_drain() {
    let source = source();
    let owner = source
        .split_once("fn net_owner_loop")
        .expect("net owner loop must exist")
        .1;
    let capture = owner
        .find("pending_rearms.push")
        .expect("masked sources must be retained by the owner");
    let wifi = owner
        .find("service_wifi_commands")
        .expect("control obligations must be serviced");
    let tx = owner
        .find(".reclaim_tx(0, NET_BATCH_LIMIT)")
        .expect("TX completion obligations must be drained");
    let rx = owner
        .find("service_rx(queues, remote)")
        .expect("RX completion obligations must be drained");
    let rearm = owner
        .find("pending_rearms.rearm_if_drained")
        .expect("drained sources must be generation-rearmed");

    assert!(
        capture < wifi && wifi < tx && tx < rx && rx < rearm,
        "source rearm must follow every bounded obligation created by its IRQ snapshot"
    );
    assert!(
        !owner.contains("parts_mut"),
        "the runtime must not split one device owner into aliasable controller and queue borrows"
    );
}

#[test]
fn every_fail_closed_park_closes_software_admission() {
    let source = source();
    let owner = source
        .split_once("fn run_net_owner")
        .expect("net owner entry must exist")
        .1
        .split_once("fn park_net_quarantine")
        .expect("net owner body must have a stable boundary")
        .0;
    assert!(owner.contains("park_net_quarantine("));
    assert!(
        !owner.contains("session.quarantine_and_park()"),
        "a direct park can strand accepted Wi-Fi requests without terminal replies"
    );

    let quarantine = source
        .split_once("fn park_net_quarantine")
        .expect("net quarantine helper must exist")
        .1;
    let close = quarantine
        .find("remote.close()")
        .expect("quarantine must close admission and drain Wi-Fi requests");
    let park = quarantine
        .find("session.quarantine_and_park()")
        .expect("quarantine helper must retain resources on the owner stack");
    assert!(
        close < park,
        "software requests must close before resources park"
    );
}
