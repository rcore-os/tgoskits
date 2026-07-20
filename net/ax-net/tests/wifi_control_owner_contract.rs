use std::{fs, path::PathBuf};

fn workspace_source(path: &str) -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(|path| path.parent())
        .expect("ax-net must remain under net/");
    fs::read_to_string(root.join(path)).expect("contract source must be readable")
}

#[test]
fn wireless_control_uses_an_immutable_runtime_facade() {
    let driver = workspace_source("net/ax-net/src/device/driver.rs");
    let ethernet = workspace_source("net/ax-net/src/device/ethernet.rs");
    let runtime = workspace_source("os/arceos/modules/axruntime/src/net.rs");

    assert!(driver.contains("trait WifiControl: Send + Sync"));
    assert!(driver.contains("fn wifi_control(&self) -> Option<Arc<dyn WifiControl>>"));
    assert!(ethernet.contains("wifi_control: Option<Arc<dyn WifiControl>>"));
    assert!(runtime.contains("struct RuntimeWifiControl"));
    assert!(!driver.contains("rd_net::"));
    assert!(!driver.contains("&mut dyn WifiControl"));
}

#[test]
fn reconfiguration_waits_without_holding_protocol_or_driver_locks() {
    let source = workspace_source("net/ax-net/src/lib.rs");
    let body = source
        .split_once("pub fn reconfigure_wifi")
        .expect("Wi-Fi reconfiguration entry must exist")
        .1
        .split_once("\n}\n")
        .expect("Wi-Fi reconfiguration body must be bounded")
        .0;

    let lookup = body
        .find("wifi_control_by_name")
        .expect("control facade must be looked up before waiting");
    let execute = body
        .find(".reconfigure(command)")
        .expect("runtime owner command must be awaited through the facade");
    let network = body
        .find("reconfigure_wifi_network")
        .expect("IP/DHCP state must change only after link completion");
    assert!(lookup < execute && execute < network);
    assert!(!body.contains("inner.driver.lock"));
}

#[test]
fn runtime_mailbox_serializes_generation_scoped_command_replies() {
    let runtime = workspace_source("os/arceos/modules/axruntime/src/net.rs");

    for required in [
        "wifi_commands",
        "WifiControlGeneration",
        "WifiCommandReply",
        "allocate_wifi_generation",
        "start_wifi_command",
        "poll_wifi_command",
    ] {
        assert!(
            runtime.contains(required),
            "missing runtime contract `{required}`"
        );
    }
    assert!(!runtime.contains("mutable_wifi_driver"));
}
