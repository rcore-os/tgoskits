/// Helper macros for power domain definition
#[macro_export(local_inner_macros)]
macro_rules! map {
    // Empty map
    () => {
        {
            ::alloc::collections::BTreeMap::new()
        }
    };
    // Multiple key-value pairs
    ( $( $key:expr => $value:expr ),+ $(,)? ) => {{
        let mut map = ::alloc::collections::BTreeMap::new();
        $( map.insert($key.into(), $value); )*
        map
    }};
}

/// Define power domain constants with documentation
macro_rules! define_power_domains {
    (
        $(
            $(#[$meta:meta])*
            $name:ident = $id:expr
        ),* $(,)?
    ) => {
        $(
            $(#[$meta])*
            pub const $name: PowerDomain = PowerDomain($id);
        )*
    };
}

/// Create a bit mask at the given position
macro_rules! bit {
    ($n:expr) => {
        (1 << $n)
    };
    () => {};
}

// Make sure RockchipDomainInfo is in scope
use super::RockchipDomainInfo;

/// Create a power domain configuration with memory, output, and repair support
///
/// # Arguments
///
/// * `name` - Domain name
/// * `pwr_offset` - Power control register offset
/// * `pwr` - Power control mask
/// * `status` - Status mask
/// * `mem_offset` - Memory power register offset
/// * `mem_status` - Memory status mask
/// * `repair_status` - Repair status mask
/// * `req_offset` - Request register offset
/// * `req` - Request mask
/// * `idle` - Idle mask
/// * `ack` - Acknowledge mask
/// * `wakeup` - Active wakeup flag
/// * `keepon` - Keep on at startup flag
#[allow(clippy::too_many_arguments)]
pub fn domain_m_o_r(
    name: &'static str,
    pwr_offset: u32,
    pwr: i32,
    status: i32,
    mem_offset: u32,
    mem_status: i32,
    repair_status: i32,
    req_offset: u32,
    req: i32,
    idle: i32,
    ack: i32,
    wakeup: bool,
    keepon: bool,
) -> RockchipDomainInfo {
    RockchipDomainInfo {
        name,
        pwr_offset,
        pwr_w_mask: (pwr << 16),
        pwr_mask: pwr,
        status_mask: status,
        mem_offset,
        mem_status_mask: mem_status,
        repair_status_mask: repair_status,
        req_offset,
        req_w_mask: (req << 16),
        req_mask: req,
        idle_mask: idle,
        ack_mask: ack,
        active_wakeup: wakeup,
        keepon_startup: keepon,
        ..Default::default()
    }
}
