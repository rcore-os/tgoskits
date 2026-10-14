//! Linux `qemu,virtual-cpufreq` compatible hint device.
//!
//! The device exposes normalized performance values only.  The run-scoped
//! `GuestCpuFreqPort` turns those values into host requests; this model never
//! accesses a physical clock, regulator, SCMI endpoint, or thermal policy.

use std::sync::{Arc, Mutex};

use axdevice::*;
use axdevice_base::{
    AccessWidth, BusKind, Device, DeviceAccess, DeviceContext, DeviceError, DeviceResult,
    DeviceVcpuId, Resource,
};
use axvmconfig::VirtualDeviceRequest;

use crate::{ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext};

pub const VIRTUAL_CPUFREQ_MODEL: &str = "virtual-cpufreq";
const MMIO_SLOT: &str = "registers";
const VCPU_STRIDE: u64 = 0x1000;
const CUR_PERF: u64 = 0x00;
const SET_PERF: u64 = 0x04;
const PERFTBL_LEN: u64 = 0x08;
const PERFTBL_SEL: u64 = 0x0c;
const PERFTBL_RD: u64 = 0x10;
const PERF_DOMAIN: u64 = 0x14;
/// Normalized guest performance ceiling (`qemu,virtual-cpufreq` `SET_PERF`).
///
/// AxVisor keeps this equal to `ax-runtime::cpufreq::GUEST_MAX_PERFORMANCE`
/// with a compile-time assert in the host adapter.
pub const VIRTUAL_CPUFREQ_MAX_PERFORMANCE: u32 = 1024;
const DEFAULT_TABLE: &[u32] = &[256, 512, 768, VIRTUAL_CPUFREQ_MAX_PERFORMANCE];

/// Catalog entry for `[[devices.virtual]] model = "virtual-cpufreq"`.
pub(super) const REGISTRATION: ConfiguredModelRegistration = ConfiguredModelRegistration {
    model: VIRTUAL_CPUFREQ_MODEL,
    create: create_device_node,
};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VirtualCpuFreqOptions {
    /// Host cpufreq domain ordinal.  It is intentionally not a physical Hz.
    pub domain: u32,
    /// Number of guest vCPU banks represented by this device.
    pub vcpu_count: u32,
    /// Normalized performance table consumed by Linux's virtual driver.
    pub performance_table: Vec<u32>,
}

impl Default for VirtualCpuFreqOptions {
    fn default() -> Self {
        Self {
            domain: 0,
            vcpu_count: 1,
            performance_table: DEFAULT_TABLE.to_vec(),
        }
    }
}

impl VirtualCpuFreqOptions {
    /// Parses and validates options at the AxVM model boundary.
    pub fn from_request(request: &VirtualDeviceRequest) -> Result<Self, String> {
        let options = request
            .deserialize_options::<Self>()
            .map_err(|error| error.to_string())?;
        options.validate().map_err(String::from)?;
        Ok(options)
    }

    /// Validates values shared by configuration materialization and device creation.
    pub fn validate(&self) -> Result<(), &'static str> {
        let vcpu_count = usize::try_from(self.vcpu_count).ok();
        if !vcpu_count.is_some_and(|count| (1..=64).contains(&count)) {
            return Err("vcpu_count must be between 1 and 64");
        }
        if self.performance_table.is_empty() || self.performance_table.len() > 64 {
            return Err("performance_table must contain between 1 and 64 entries");
        }
        if self
            .performance_table
            .iter()
            .any(|value| *value == 0 || *value > VIRTUAL_CPUFREQ_MAX_PERFORMANCE)
            || self
                .performance_table
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err("performance_table must be strictly increasing in 1..=1024");
        }
        Ok(())
    }

    /// Replaces the host-materialized table while retaining the model-owned schema validation.
    pub fn replace_performance_table(
        request: &mut VirtualDeviceRequest,
        performance_table: Vec<u32>,
    ) -> Result<(), String> {
        let mut options = Self::from_request(request)?;
        options.performance_table = performance_table;
        options.validate().map_err(String::from)?;
        request.options.insert(
            String::from("performance_table"),
            toml::Value::Array(
                options
                    .performance_table
                    .into_iter()
                    .map(|value| toml::Value::Integer(i64::from(value)))
                    .collect(),
            ),
        );
        Ok(())
    }
}

fn invalid_options(
    request: &VirtualDeviceRequest,
    detail: impl Into<String>,
) -> ConfiguredDeviceError {
    ConfiguredDeviceError::InvalidOptions {
        device: request.id.clone(),
        model: request.model.clone(),
        detail: detail.into(),
    }
}

fn create_device_node(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    _context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    let options = VirtualCpuFreqOptions::from_request(request)
        .map_err(|error| invalid_options(request, error))?;
    let vcpu_count = usize::try_from(options.vcpu_count).expect("validated vcpu_count");
    Ok(DeviceNodeSpec::virtual_device(
        id,
        Arc::new(VirtualCpuFreqModel {
            domain: options.domain,
            vcpu_count,
            performance_table: options.performance_table,
        }),
    ))
}

struct VirtualCpuFreqModel {
    domain: u32,
    vcpu_count: usize,
    performance_table: Vec<u32>,
}

impl DeviceModel for VirtualCpuFreqModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        let size = VCPU_STRIDE.checked_mul(self.vcpu_count as u64).ok_or(
            DeviceManagerError::InvalidConfig {
                operation: "declare virtual CPU frequency MMIO",
                detail: "vCPU bank size overflows".into(),
            },
        )?;
        DeviceRequirements::new().with_mmio(
            ResourceSlot::new(MMIO_SLOT)?,
            size,
            VCPU_STRIDE,
            ResourceRequest::Auto,
        )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::interfaces(
            Some(vec![FdtContributionSpec::Conventional(
                FdtNodeSpec::new("cpufreq")
                    .with_compatible("qemu,virtual-cpufreq")
                    .with_register(ResourceSlot::new(MMIO_SLOT).expect("static slot is valid"))
                    .with_string_property("status", "okay"),
            )]),
            None,
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let (base, length) = context.mmio(MMIO_SLOT)?;
        let expected = VCPU_STRIDE * self.vcpu_count as u64;
        if length != expected {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build virtual CPU frequency device",
                detail: format!("MMIO length {length:#x} does not equal {expected:#x}"),
            });
        }
        let device = VirtualCpuFreqDevice {
            resources: vec![Resource::MmioRange { base, size: length }],
            vcpu_count: self.vcpu_count,
            domain: self.domain,
            performance_table: self.performance_table.clone().into_boxed_slice(),
            table_selection: Mutex::new(vec![0; self.vcpu_count]),
        };
        let mut bundle = DeviceBundle::new();
        bundle.add_device(Arc::new(device));
        Ok(bundle)
    }
}

struct VirtualCpuFreqDevice {
    resources: Vec<Resource>,
    vcpu_count: usize,
    domain: u32,
    performance_table: Box<[u32]>,
    table_selection: Mutex<Vec<usize>>,
}

impl VirtualCpuFreqDevice {
    fn register(&self, access: &DeviceAccess) -> DeviceResult<(usize, u64)> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::Unsupported {
                operation: "access virtual CPU frequency device",
                detail: "only MMIO accesses are supported".into(),
            });
        }
        if access.width() != AccessWidth::Dword {
            return Err(DeviceError::InvalidWidth {
                expected: AccessWidth::Dword,
                actual: access.width(),
            });
        }
        let Resource::MmioRange { base, size } = self.resources[0] else {
            unreachable!("virtual CPU frequency device has one MMIO resource")
        };
        let offset = access
            .address()
            .checked_sub(base)
            .ok_or(DeviceError::OutOfRange {
                addr: access.address(),
            })?;
        if offset >= size {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        let vcpu = usize::try_from(offset / VCPU_STRIDE).map_err(|_| DeviceError::OutOfRange {
            addr: access.address(),
        })?;
        if vcpu >= self.vcpu_count {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        Ok((vcpu, offset % VCPU_STRIDE))
    }
}

impl Device for VirtualCpuFreqDevice {
    fn name(&self) -> &str {
        VIRTUAL_CPUFREQ_MODEL
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(&self, access: &DeviceAccess, context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        let (vcpu, register) = self.register(access)?;
        match register {
            CUR_PERF => context
                .current_cpu_freq_performance(DeviceVcpuId::new(vcpu), self.domain)
                .map(u64::from),
            PERFTBL_LEN => Ok(self.performance_table.len() as u64),
            PERFTBL_SEL => Ok(self.table_selection.lock().unwrap()[vcpu] as u64),
            PERFTBL_RD => {
                let selection = self.table_selection.lock().unwrap()[vcpu];
                Ok(self.performance_table.get(selection).copied().unwrap_or(0) as u64)
            }
            PERF_DOMAIN => Ok(self.domain as u64),
            SET_PERF => Err(DeviceError::ReadOnly),
            _ => Err(DeviceError::OutOfRange {
                addr: access.address(),
            }),
        }
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        let (vcpu, register) = self.register(access)?;
        let value = u32::try_from(value).map_err(|_| DeviceError::InvalidInput {
            operation: "write virtual CPU frequency register",
            detail: "value does not fit a 32-bit register".into(),
        })?;
        match register {
            SET_PERF => {
                if !(1..=VIRTUAL_CPUFREQ_MAX_PERFORMANCE).contains(&value) {
                    return Err(DeviceError::InvalidInput {
                        operation: "write virtual CPU frequency performance",
                        detail: "normalized performance must be in 1..=1024".into(),
                    });
                }
                context.set_cpu_freq_hint(DeviceVcpuId::new(vcpu), self.domain, value)
            }
            PERFTBL_SEL => {
                let selection = usize::try_from(value).map_err(|_| DeviceError::InvalidInput {
                    operation: "select virtual CPU frequency table entry",
                    detail: "table index does not fit usize".into(),
                })?;
                if selection >= self.performance_table.len() {
                    return Err(DeviceError::InvalidInput {
                        operation: "select virtual CPU frequency table entry",
                        detail: "table index is out of range".into(),
                    });
                }
                self.table_selection.lock().unwrap()[vcpu] = selection;
                Ok(())
            }
            CUR_PERF | PERFTBL_LEN | PERFTBL_RD | PERF_DOMAIN => Err(DeviceError::ReadOnly),
            _ => Err(DeviceError::OutOfRange {
                addr: access.address(),
            }),
        }
    }
}

pub(super) fn register(
    catalog: &mut crate::ConfiguredDeviceCatalog,
) -> Result<(), ConfiguredDeviceError> {
    catalog.register(module_path!(), REGISTRATION)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axdevice_base::{DeviceId, DeviceVcpuId};

    use super::*;

    struct TestContext {
        hints: Mutex<Vec<(usize, u32, u32)>>,
        current_vcpus: Mutex<Vec<usize>>,
        current: u32,
    }

    impl DeviceContext for TestContext {
        fn device_id(&self) -> DeviceId {
            DeviceId::new(7)
        }

        fn set_cpu_freq_hint(
            &mut self,
            vcpu_id: DeviceVcpuId,
            domain: u32,
            performance: u32,
        ) -> DeviceResult {
            self.hints
                .lock()
                .unwrap()
                .push((vcpu_id.as_usize(), domain, performance));
            Ok(())
        }

        fn current_cpu_freq_performance(
            &mut self,
            vcpu_id: DeviceVcpuId,
            _domain: u32,
        ) -> DeviceResult<u32> {
            self.current_vcpus.lock().unwrap().push(vcpu_id.as_usize());
            Ok(self.current)
        }
    }

    fn device() -> VirtualCpuFreqDevice {
        VirtualCpuFreqDevice {
            resources: vec![Resource::MmioRange {
                base: 0x1000,
                size: VCPU_STRIDE * 2,
            }],
            vcpu_count: 2,
            domain: 1,
            performance_table: Box::new([256, 512, 1024]),
            table_selection: Mutex::new(vec![0, 0]),
        }
    }

    fn access(vcpu: usize, offset: u64) -> DeviceAccess {
        DeviceAccess::new(
            DeviceVcpuId::new(vcpu),
            BusKind::Mmio,
            0x1000 + vcpu as u64 * VCPU_STRIDE + offset,
            AccessWidth::Dword,
        )
    }

    #[test]
    fn model_schema_owns_defaults_and_materialized_table_validation() {
        let mut request: axvmconfig::VirtualDeviceRequest = toml::from_str(
            r#"
                id = "cpu"
                model = "virtual-cpufreq"
                domain = 1
            "#,
        )
        .unwrap();
        let options = VirtualCpuFreqOptions::from_request(&request).unwrap();
        assert_eq!(options.domain, 1);
        assert_eq!(options.vcpu_count, 1);
        assert_eq!(
            options.performance_table,
            [256, 512, 768, VIRTUAL_CPUFREQ_MAX_PERFORMANCE]
        );
        assert_eq!(
            *options.performance_table.last().unwrap(),
            VIRTUAL_CPUFREQ_MAX_PERFORMANCE
        );

        VirtualCpuFreqOptions::replace_performance_table(&mut request, vec![408, 816, 1024])
            .unwrap();
        assert_eq!(
            VirtualCpuFreqOptions::from_request(&request)
                .unwrap()
                .performance_table,
            [408, 816, 1024]
        );

        request.options.insert(
            String::from("performance_table"),
            toml::Value::Array(vec![toml::Value::Integer(1024), toml::Value::Integer(512)]),
        );
        assert!(VirtualCpuFreqOptions::from_request(&request).is_err());
    }

    #[test]
    fn mmio_bank_layout_and_fdt_node_are_the_declared_abi() {
        let model = VirtualCpuFreqModel {
            domain: 1,
            vcpu_count: 2,
            performance_table: vec![256, 512, 1024],
        };
        let requirements = model.requirements().unwrap();
        assert_eq!(requirements.entries().len(), 1);
        let DeviceRequirement::Mmio {
            slot,
            size,
            alignment,
            ..
        } = &requirements.entries()[0]
        else {
            panic!("virtual CPU frequency device declares one MMIO window");
        };
        // Upstream addresses a vCPU bank as `cpu * PER_CPU_OFFSET`, so the
        // window length and its stride are a binary interface contract.
        assert_eq!(slot.as_str(), MMIO_SLOT);
        assert_eq!(*size, VCPU_STRIDE * 2);
        assert_eq!(*alignment, VCPU_STRIDE);

        let firmware = model.firmware();
        let [FdtContributionSpec::Conventional(node)] = firmware
            .fdt()
            .expect("virtual CPU frequency device describes FDT")
        else {
            panic!("virtual CPU frequency device contributes one conventional FDT node");
        };
        assert_eq!(node.compatible(), &[String::from("qemu,virtual-cpufreq")]);
        assert_eq!(node.register_slots().len(), 1);
        assert_eq!(node.register_slots()[0].as_str(), MMIO_SLOT);
        assert!(node.properties().iter().any(|property| matches!(
            property,
            DeviceFirmwareProperty::String { name, value }
                if name.as_str() == "status" && value.as_str() == "okay"
        )));
    }

    #[test]
    fn exposes_upstream_register_banks_and_forwards_set_perf() {
        let device = device();
        let mut context = TestContext {
            hints: Mutex::new(Vec::new()),
            current_vcpus: Mutex::new(Vec::new()),
            current: 768,
        };
        assert_eq!(device.read(&access(1, PERFTBL_LEN), &mut context), Ok(3));
        device
            .write(&access(1, PERFTBL_SEL), 2, &mut context)
            .unwrap();
        assert_eq!(device.read(&access(1, PERFTBL_RD), &mut context), Ok(1024));
        device
            .write(&access(1, SET_PERF), 900, &mut context)
            .unwrap();
        assert_eq!(context.hints.lock().unwrap().as_slice(), &[(1, 1, 900)]);
        assert_eq!(device.read(&access(1, CUR_PERF), &mut context), Ok(768));
        assert_eq!(device.read(&access(1, PERF_DOMAIN), &mut context), Ok(1));
    }

    #[test]
    fn rejects_invalid_width_values_and_table_indices() {
        let device = device();
        let mut context = TestContext {
            hints: Mutex::new(Vec::new()),
            current_vcpus: Mutex::new(Vec::new()),
            current: 0,
        };
        let byte_access = DeviceAccess::new(
            DeviceVcpuId::new(0),
            BusKind::Mmio,
            0x1000 + SET_PERF,
            AccessWidth::Byte,
        );
        assert!(matches!(
            device.write(&byte_access, 1, &mut context),
            Err(DeviceError::InvalidWidth { .. })
        ));
        assert!(matches!(
            device.write(&access(0, PERFTBL_SEL), 3, &mut context),
            Err(DeviceError::InvalidInput { .. })
        ));
        assert!(matches!(
            device.write(
                &access(0, SET_PERF),
                u64::from(VIRTUAL_CPUFREQ_MAX_PERFORMANCE) + 1,
                &mut context
            ),
            Err(DeviceError::InvalidInput { .. })
        ));
        assert!(matches!(
            device.write(&access(0, SET_PERF), 0, &mut context),
            Err(DeviceError::InvalidInput { .. })
        ));
    }

    #[test]
    fn register_bank_identity_is_used_for_frequency_hints() {
        let device = device();
        let mut context = TestContext {
            hints: Mutex::new(Vec::new()),
            current_vcpus: Mutex::new(Vec::new()),
            current: 768,
        };
        let bank_one_from_vcpu_zero = DeviceAccess::new(
            DeviceVcpuId::new(0),
            BusKind::Mmio,
            0x1000 + VCPU_STRIDE + CUR_PERF,
            AccessWidth::Dword,
        );
        assert_eq!(device.read(&bank_one_from_vcpu_zero, &mut context), Ok(768));
        device
            .write(
                &DeviceAccess::new(
                    DeviceVcpuId::new(0),
                    BusKind::Mmio,
                    0x1000 + VCPU_STRIDE + SET_PERF,
                    AccessWidth::Dword,
                ),
                900,
                &mut context,
            )
            .unwrap();
        assert_eq!(context.current_vcpus.lock().unwrap().as_slice(), &[1]);
        assert_eq!(context.hints.lock().unwrap().as_slice(), &[(1, 1, 900)]);
    }
}
