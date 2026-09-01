//! Log-level-independent startup address for the board-hosted web console.

use core::fmt::Write as _;
use std::{
    net::{IpAddr, SocketAddr},
    string::{String, ToString},
    thread,
    time::Duration,
};

use ax_std::os::arceos::modules::ax_net::{self, InterfaceKind, Ipv4InterfaceConfig};

use crate::guest_console;

const ADDRESS_POLL_INTERVAL: Duration = Duration::from_millis(250);

struct InterfaceAddress {
    name: String,
    ipv4: Ipv4InterfaceConfig,
}

/// Prints the first usable web-console address and then stops monitoring.
pub(crate) fn start() {
    if let Some(interface) = ready_interface() {
        submit_access_banner(&interface);
        return;
    }

    guest_console::submit_host_bytes(
        b"\r\nAxvisor network waiting:\r\n\
          no reachable web console address yet\r\n\
          VM startup will continue while network configuration completes\r\n",
    );
    if let Err(error) = thread::Builder::new()
        .name("axvisor-network-status".into())
        .spawn(wait_for_ready_interface)
    {
        let message = format!(
            "\r\nAxvisor web console address monitor unavailable:\r\n  error = {error}\r\n"
        );
        guest_console::submit_host_bytes(message.as_bytes());
    }
}

fn wait_for_ready_interface() {
    loop {
        thread::sleep(ADDRESS_POLL_INTERVAL);
        if let Some(interface) = ready_interface() {
            submit_access_banner(&interface);
            return;
        }
    }
}

fn ready_interface() -> Option<InterfaceAddress> {
    if !crate::http::is_listening() {
        return None;
    }
    ax_net::interfaces().into_iter().find_map(|interface| {
        (interface.kind != InterfaceKind::Loopback)
            .then_some(interface.ipv4)
            .flatten()
            .map(|ipv4| InterfaceAddress {
                name: interface.name,
                ipv4,
            })
    })
}

fn submit_access_banner(interface: &InterfaceAddress) {
    let address = interface.ipv4.address.address();
    let mut banner = String::new();
    let _ = write!(banner, "\r\nAxvisor network ready:\r\n");
    let _ = write!(banner, "  interface = {}\r\n", interface.name);
    let _ = write!(
        banner,
        "  ipv4 = {address}/{}\r\n",
        interface.ipv4.address.prefix_len()
    );
    append_web_console_endpoint(&mut banner, address, crate::http::bind_addr());
    guest_console::submit_host_bytes(banner.as_bytes());
}

fn append_web_console_endpoint(
    banner: &mut String,
    assigned_address: impl core::fmt::Display,
    bind: &str,
) {
    let Ok(bind) = bind.parse::<SocketAddr>() else {
        return;
    };
    let host = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => assigned_address.to_string(),
        ip => ip.to_string(),
    };
    let _ = write!(banner, "  web_console = http://{host}:{}/\r\n", bind.port());
}
