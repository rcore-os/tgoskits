#![no_std]
#![no_main]

use ax_net as _;
use ax_std as _;
use axtest::prelude::*;

#[axtest]
fn initialization_keeps_task_context_locking_preemptible() {
    ax_net::axtest_support::run_initialization_contracts();
}

#[axtest]
fn udp_bind_ownership_matches_target_locking() {
    ax_net::axtest_support::run_udp_bind_contracts();
}

#[axtest]
fn udp_route_selection_uses_the_initialized_runtime() {
    ax_net::axtest_support::run_udp_route_contracts();
}

#[axtest]
fn tcp_options_routes_and_port_ownership_hold() {
    ax_net::axtest_support::run_tcp_contracts();
}

#[axtest]
fn router_frame_accounting_uses_target_synchronization() {
    ax_net::axtest_support::run_router_accounting_contracts();
}

#[axtest]
fn vsock_connection_ownership_uses_target_synchronization() {
    ax_net::axtest_support::run_vsock_connection_contracts();
}

#[axtest]
fn vsock_polling_releases_gates_and_obeys_its_budget() {
    ax_net::axtest_support::run_vsock_poll_contracts();
}

#[axtest::tests]
mod tests {}
