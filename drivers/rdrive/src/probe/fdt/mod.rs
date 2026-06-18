use alloc::{
    collections::{BTreeMap, btree_set::BTreeSet},
    string::String,
    vec::Vec,
};
use core::ptr::NonNull;

pub use fdt_edit::{ClockRef, Fdt, InterruptRef, NodeId, NodeType, Phandle, RegInfo, Status};
use spin::{Mutex, Once};

use super::ProbeError;
use crate::{
    Descriptor, DeviceId, PlatformDevice,
    error::DriverError,
    probe::OnProbeError,
    register::{DriverRegister, ProbeKind},
};

static SYSTEM: Once<System> = Once::new();

pub fn init(fdt_addr: NonNull<u8>) -> Result<(), DriverError> {
    let sys = System::new(fdt_addr)?;
    SYSTEM.call_once(|| sys);
    Ok(())
}

pub fn check_addr(fdt_addr: NonNull<u8>) -> Result<(), DriverError> {
    unsafe { Fdt::from_ptr(fdt_addr.as_ptr()) }
        .map(|_| ())
        .map_err(|error| DriverError::Fdt(format!("{error:?}")))
}

pub fn probe_register(
    register: &DriverRegister,
) -> Result<Vec<Result<(), OnProbeError>>, ProbeError> {
    let sys = system();
    sys.probe_register(register)
}

pub(crate) fn try_probe_register(
    register: &DriverRegister,
) -> Option<Result<Vec<Result<(), OnProbeError>>, ProbeError>> {
    SYSTEM.get().map(|system| system.probe_register(register))
}

pub(crate) fn system() -> &'static System {
    SYSTEM.get().expect("rdrive not init")
}

pub(crate) fn try_system() -> Option<&'static System> {
    SYSTEM.get()
}

pub struct FdtInfo<'a> {
    pub node: NodeType<'a>,
    phandle_2_device_id: BTreeMap<Phandle, DeviceId>,
}

impl<'a> FdtInfo<'a> {
    pub fn get_by_phandle(&self, phandle: Phandle) -> Option<NodeType<'a>> {
        system().get_by_phandle(phandle)
    }

    pub fn find_compatible(&self, compatible: &[&str]) -> Vec<NodeType<'a>> {
        system().find_compatible(compatible)
    }

    pub fn phandle_to_device_id(&self, phandle: Phandle) -> Option<DeviceId> {
        self.phandle_2_device_id.get(&phandle).copied()
    }

    pub fn find_clk_by_name(&self, name: &str) -> Option<ClockRef> {
        self.node
            .clocks()
            .into_iter()
            .find(|clock| clock.name.as_deref() == Some(name))
    }

    pub fn interrupts(&self) -> Vec<InterruptRef> {
        self.node.interrupts()
    }
}

pub struct ProbeFdt<'a> {
    info: FdtInfo<'a>,
    platform: PlatformDevice,
}

impl<'a> ProbeFdt<'a> {
    pub(crate) fn new(info: FdtInfo<'a>, platform: PlatformDevice) -> Self {
        Self { info, platform }
    }

    pub const fn info(&self) -> &FdtInfo<'a> {
        &self.info
    }

    pub fn into_platform_device(self) -> PlatformDevice {
        self.platform
    }

    pub fn into_parts(self) -> (FdtInfo<'a>, PlatformDevice) {
        (self.info, self.platform)
    }
}

pub type FnOnProbe = for<'a> fn(ProbeFdt<'a>) -> Result<(), OnProbeError>;

pub struct System {
    fdt: Fdt,
    phandle_2_device_id: BTreeMap<Phandle, DeviceId>,
    populated_paths: Mutex<BTreeMap<String, DeviceId>>,
    populated_nodes: Mutex<BTreeSet<NodeId>>,
}

unsafe impl Send for System {}

impl System {
    pub fn fdt(&self) -> &Fdt {
        &self.fdt
    }

    pub fn phandle_to_device_id(&self, phandle: Phandle) -> Option<DeviceId> {
        self.phandle_2_device_id.get(&phandle).copied()
    }

    pub fn path_to_device_id(&self, path: &str) -> Option<DeviceId> {
        self.populated_paths.lock().get(path).copied()
    }

    pub fn get_by_phandle(&self, phandle: Phandle) -> Option<NodeType<'_>> {
        self.fdt.get_by_phandle(phandle)
    }

    pub fn find_compatible(&self, compatible: &[&str]) -> Vec<NodeType<'_>> {
        self.fdt.find_compatible(compatible)
    }

    pub fn new(fdt_addr: NonNull<u8>) -> Result<Self, DriverError> {
        let fdt = unsafe { Fdt::from_ptr(fdt_addr.as_ptr()) }
            .map_err(|error| DriverError::Fdt(format!("{error:?}")))?;
        let mut phandle_2_device_id = BTreeMap::new();
        for node in fdt.all_nodes() {
            if let Some(phandle) = node.as_node().phandle() {
                phandle_2_device_id.insert(phandle, DeviceId::new());
            }
        }
        Ok(Self {
            fdt,
            phandle_2_device_id,
            populated_paths: Mutex::new(BTreeMap::new()),
            populated_nodes: Mutex::new(BTreeSet::new()),
        })
    }

    fn new_device_id(&self, phandle: Option<Phandle>) -> DeviceId {
        if let Some(phandle) = phandle {
            self.phandle_2_device_id[&phandle]
        } else {
            DeviceId::new()
        }
    }

    fn get_fdt_match_nodes<'a>(&'a self, register: &DriverRegister) -> Vec<ProbeFdtInfo<'a>> {
        let mut out = Vec::new();
        for node in self.fdt.all_nodes() {
            if matches!(node.as_node().status(), Some(Status::Disabled)) {
                continue;
            }

            let node_compatibles = node.as_node().compatibles().collect::<Vec<_>>();

            for probe in register.probe_kinds {
                let &ProbeKind::Fdt {
                    compatibles,
                    on_probe,
                } = probe
                else {
                    continue;
                };

                for compatible in &node_compatibles {
                    if compatibles.contains(compatible) {
                        out.push(ProbeFdtInfo {
                            name: register.name,
                            node,
                            on_probe,
                        });
                    }
                }
            }
        }
        out
    }

    fn probe_register(
        &self,
        register: &DriverRegister,
    ) -> Result<Vec<Result<(), OnProbeError>>, ProbeError> {
        let node_ls = self.get_fdt_match_nodes(register);
        let mut out = Vec::new();
        for node_info in node_ls {
            let node_id = node_info.node.id();
            if self.populated_nodes.lock().contains(&node_id) {
                continue;
            }
            let node = node_info.node;
            let node_phandle = node.as_node().phandle();
            let id = self.new_device_id(node_phandle);

            let irq_parent = node
                .interrupt_parent()
                .filter(|p| Some(*p) != node_phandle)
                .and_then(|p| self.phandle_2_device_id.get(&p).copied());

            let phandle_map = self.phandle_2_device_id.clone();

            debug!("Probe [{}]->[{}]", node.name(), node_info.name);

            let descriptor = Descriptor {
                name: node_info.name,
                device_id: id,
                irq_parent,
            };

            let res = (node_info.on_probe)(ProbeFdt::new(
                FdtInfo {
                    node,
                    phandle_2_device_id: phandle_map,
                },
                PlatformDevice::new(descriptor),
            ));

            if res.is_ok() {
                self.populated_paths.lock().insert(node.path(), id);
                self.populated_nodes.lock().insert(node_id);
            }

            out.push(res);
        }

        Ok(out)
    }
}

struct ProbeFdtInfo<'a> {
    name: &'static str,
    node: NodeType<'a>,
    on_probe: FnOnProbe,
}
