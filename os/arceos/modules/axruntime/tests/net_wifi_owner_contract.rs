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
fn queued_command_cannot_spin_an_active_command_waiting_for_irq() {
    let source = source();
    assert!(
        source.contains("fn wifi_command_work_ready("),
        "the owner loop needs one readiness predicate that serializes queued commands behind the \
         active command"
    );
    assert!(
        source.contains("wifi_command_work_ready(&active_wifi_command"),
        "both park and rerun decisions must use the serialized Wi-Fi readiness predicate"
    );
}

#[test]
fn command_admission_rechecks_shutdown_while_holding_the_mailbox_lock() {
    let source = source();
    let submit = source
        .split_once("fn submit_wifi_command(")
        .expect("Wi-Fi mailbox submit path must exist")
        .1
        .split_once("fn allocate_wifi_generation")
        .expect("Wi-Fi mailbox submit body must have a stable boundary")
        .0;
    let lock = submit
        .find("let mut commands = self.wifi_commands.lock();")
        .expect("command admission must hold the mailbox lock");
    let recheck = submit[lock..]
        .find("self.closed.load(Ordering::Acquire)")
        .expect("command admission must recheck shutdown under the mailbox lock");
    let push = submit[lock..]
        .find("commands.push_back(request)")
        .expect("command admission must publish into the mailbox");
    assert!(
        recheck < push,
        "shutdown must be checked before publication"
    );
}

#[test]
fn command_generation_is_allocated_in_fifo_admission_order() {
    let source = source();
    let submit = source
        .split_once("fn submit_wifi_command(")
        .expect("Wi-Fi mailbox submit path must exist")
        .1
        .split_once("fn allocate_wifi_generation")
        .expect("Wi-Fi mailbox submit body must have a stable boundary")
        .0;
    let lock = submit
        .find("let mut commands = self.wifi_commands.lock();")
        .expect("command admission must hold the FIFO mailbox lock");
    let generation = submit
        .find("allocate_wifi_generation")
        .expect("command admission must allocate a generation");
    let push = submit
        .find("commands.push_back(request)")
        .expect("command admission must publish into the FIFO mailbox");
    assert!(
        lock < generation && generation < push,
        "generation order must be identical to FIFO command order"
    );
}

#[test]
fn typed_completion_carries_generation_into_protocol_commit() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .and_then(|path| path.parent())
        .and_then(|path| path.parent())
        .expect("ax-runtime must remain under os/arceos/modules")
        .to_path_buf();
    let ax_net_root = root.join("net/ax-net/src");
    let driver = fs::read_to_string(ax_net_root.join("device/driver.rs"))
        .expect("ax-net wireless facade must be readable");
    let net = fs::read_to_string(ax_net_root.join("lib.rs"))
        .expect("ax-net control transaction must be readable");
    let router = fs::read_to_string(ax_net_root.join("router.rs"))
        .expect("ax-net router generation gate must be readable");

    assert!(driver.contains("pub struct WifiControlCompletion"));
    assert!(net.contains("completion.generation"));
    assert!(router.contains("accept_wifi_generation"));
}
