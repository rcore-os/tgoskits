use core::fmt::Display;

use aarch64_cpu::asm::wfi;
use kernutil::StaticCell;
use smccc::{Hvc, Smc, psci};

static METHOD: StaticCell<Method> = StaticCell::uninit();

pub(crate) fn init() {
    let fdt = crate::fdt::fdt().unwrap();

    let nodes = fdt.find_compatible(&["arm,psci-1.0", "arm,psci-0.2", "arm,psci"]);

    let method: Method = nodes[0]
        .as_node()
        .get_property("method")
        .unwrap()
        .as_str()
        .unwrap()
        .into();

    METHOD.init(method);
    info!("Power management method : {method}");
}

#[derive(Debug, Clone, Copy)]
enum Method {
    Smc,
    Hvc,
}
impl From<&str> for Method {
    fn from(value: &str) -> Self {
        match value {
            "smc" => Method::Smc,
            "hvc" => Method::Hvc,
            _ => {
                panic!("Unsupported power method: {}", value);
            }
        }
    }
}
impl Display for Method {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Method::Smc => write!(f, "SMC"),
            Method::Hvc => write!(f, "HVC"),
        }
    }
}

// Shutdown the system
pub fn shutdown() -> ! {
    if !METHOD.is_init() {
        loop {
            wfi();
        }
    }

    if let Err(e) = match *METHOD {
        Method::Smc => psci::system_off::<Smc>(),
        Method::Hvc => psci::system_off::<Hvc>(),
    } {
        println!("Failed to shutdown: {e}");
    }
    loop {
        wfi();
    }
}

pub(crate) fn cpu_on(
    cpu_id: u64,
    entry: u64,
    stack_top: u64,
) -> Result<(), smccc::psci::error::Error> {
    let method = *METHOD;
    debug!("[{method}]Power on CPU {cpu_id:#x} at entry {entry:#x}, stack top {stack_top:#x}",);
    match method {
        Method::Smc => psci::cpu_on::<Smc>(cpu_id, entry, stack_top)?,
        Method::Hvc => psci::cpu_on::<Hvc>(cpu_id, entry, stack_top)?,
    };
    Ok(())
}
