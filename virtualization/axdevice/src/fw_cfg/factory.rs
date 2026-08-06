use super::*;

/// Runtime adapter that gives fw_cfg DMA access only to the current bus access.
pub struct FwCfgDmaDevice {
    inner: Arc<FwCfg>,
    dma_grant: DmaGrant,
    name: String,
    resources: Box<[Resource]>,
}

/// Runtime image payload used to build one fw_cfg device contribution.
///
/// fw_cfg is supplied by the boot loader rather than guest TOML: its kernel,
/// initrd and command-line bytes are VM image state. Keeping that input typed
/// prevents it from becoming an architecture-side registration special case.
pub struct FwCfgBuildConfig {
    /// Guest MMIO base address.
    pub base: GuestPhysAddr,
    /// Guest MMIO region size.
    pub size: usize,
    /// Kernel image exposed through fw_cfg.
    pub kernel: FwCfgKernelPayload,
    /// Optional initrd image exposed through fw_cfg.
    pub initrd: Option<Arc<[u8]>>,
    /// Optional kernel command line.
    pub cmdline: Option<String>,
    /// Number of guest CPUs.
    pub cpu_num: u16,
    /// Platform firmware description inputs.
    pub platform: FwCfgPlatformConfig,
}

/// Builds fw_cfg as a normal capability-declaring device contribution.
pub struct FwCfgDeviceFactory;

/// VM-local factory input supplied by the boot-image loader.
#[derive(Clone)]
pub struct FwCfgPayloadConfig {
    pub base: GuestPhysAddr,
    pub size: usize,
    pub kernel: FwCfgKernelPayload,
    pub initrd: Option<Arc<[u8]>>,
    pub cmdline: Option<String>,
    pub cpu_num: u16,
    pub platform: FwCfgPlatformConfig,
}

/// Factory that joins boot-image payloads to the configured fw_cfg device.
pub struct FwCfgPayloadFactory {
    payload: FwCfgPayloadSource,
    transport: FwCfgTransport,
    base: GuestPhysAddr,
    size: usize,
}

#[derive(Clone, Copy)]
enum FwCfgTransport {
    Mmio,
    Pio,
}

enum FwCfgPayloadSource {
    Fixed(FwCfgPayloadConfig),
    Deferred(Arc<FwCfgPayloadSlot>),
}

/// Once-populated boot payload shared with an early-declared fw_cfg factory.
pub struct FwCfgPayloadSlot {
    payload: Mutex<Option<FwCfgPayloadConfig>>,
}

impl FwCfgPayloadSlot {
    /// Creates an empty payload slot.
    pub const fn new() -> Self {
        Self {
            payload: Mutex::new(None),
        }
    }

    /// Installs the only payload accepted for this VM.
    pub fn set(&self, payload: FwCfgPayloadConfig) -> DeviceManagerResult {
        let mut current = self.payload.lock();
        if current.is_some() {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "install fw_cfg boot payload",
                detail: "the VM already has an fw_cfg payload".into(),
            });
        }
        *current = Some(payload);
        Ok(())
    }

    /// Returns the current immutable payload snapshot.
    pub fn get(&self) -> Option<FwCfgPayloadConfig> {
        self.payload.lock().clone()
    }
}

impl Default for FwCfgPayloadSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl FwCfgPayloadFactory {
    pub const fn new(payload: FwCfgPayloadConfig) -> Self {
        Self {
            base: payload.base,
            size: payload.size,
            payload: FwCfgPayloadSource::Fixed(payload),
            transport: FwCfgTransport::Mmio,
        }
    }

    /// Creates an x86 PIO factory from an already available boot payload.
    pub const fn new_pio(payload: FwCfgPayloadConfig) -> Self {
        Self {
            base: payload.base,
            size: payload.size,
            payload: FwCfgPayloadSource::Fixed(payload),
            transport: FwCfgTransport::Pio,
        }
    }

    /// Creates a factory whose payload is populated by the boot loader later.
    pub const fn deferred(
        base: GuestPhysAddr,
        size: usize,
        payload: Arc<FwCfgPayloadSlot>,
    ) -> Self {
        Self {
            payload: FwCfgPayloadSource::Deferred(payload),
            transport: FwCfgTransport::Mmio,
            base,
            size,
        }
    }

    /// Creates an x86 PIO factory whose payload is populated by the boot loader.
    pub const fn deferred_pio(
        base: GuestPhysAddr,
        size: usize,
        payload: Arc<FwCfgPayloadSlot>,
    ) -> Self {
        Self {
            payload: FwCfgPayloadSource::Deferred(payload),
            transport: FwCfgTransport::Pio,
            base,
            size,
        }
    }

    fn payload(&self) -> DeviceManagerResult<FwCfgPayloadConfig> {
        match &self.payload {
            FwCfgPayloadSource::Fixed(payload) => Ok(payload.clone()),
            FwCfgPayloadSource::Deferred(slot) => {
                slot.get().ok_or_else(|| DeviceManagerError::InvalidState {
                    operation: "build fw_cfg device",
                    detail: "the boot loader has not installed an fw_cfg payload".into(),
                })
            }
        }
    }
}

impl DeviceModel for FwCfgPayloadFactory {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        let requirements = match self.transport {
            FwCfgTransport::Mmio => {
                let size = u64::try_from(self.size).map_err(fw_cfg_range_conversion_error)?;
                let base =
                    u64::try_from(self.base.as_usize()).map_err(fw_cfg_range_conversion_error)?;
                DeviceRequirements::new().with_mmio(
                    ResourceSlot::new("registers")?,
                    size,
                    1,
                    ResourceRequest::Fixed(base),
                )?
            }
            FwCfgTransport::Pio => pio_requirements(self.base, self.size)?,
        };
        Ok(requirements)
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let payload = self.payload()?;
        if self.base != payload.base || self.size != payload.size {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build fw_cfg device",
                detail: format!(
                    "configured range [{:#x}, {:#x}) differs from boot payload range [{:#x}, \
                     {:#x})",
                    self.base.as_usize(),
                    self.base.as_usize().saturating_add(self.size),
                    payload.base.as_usize(),
                    payload.base.as_usize().saturating_add(payload.size),
                ),
            });
        }
        let build = FwCfgBuildConfig {
            base: payload.base,
            size: payload.size,
            kernel: payload.kernel,
            initrd: payload.initrd,
            cmdline: payload.cmdline,
            cpu_num: payload.cpu_num,
            platform: payload.platform,
        };
        match self.transport {
            FwCfgTransport::Mmio => {
                let (base, size) = context.mmio(&ResourceSlot::new("registers")?)?;
                let base = usize::try_from(base).map_err(fw_cfg_range_conversion_error)?;
                let size = usize::try_from(size).map_err(fw_cfg_range_conversion_error)?;
                FwCfgDeviceFactory::new().build(FwCfgBuildConfig {
                    base: GuestPhysAddr::from_usize(base),
                    size,
                    ..build
                })
            }
            FwCfgTransport::Pio => {
                let (selector_base, selector_size) =
                    context.pio(&ResourceSlot::new("selector-data")?)?;
                let (dma_base, dma_size) = context.pio(&ResourceSlot::new("dma")?)?;
                FwCfgDeviceFactory::new().build_pio(
                    selector_base,
                    selector_size,
                    dma_base,
                    dma_size,
                    build,
                )
            }
        }
    }
}

fn pio_requirements(base: GuestPhysAddr, size: usize) -> DeviceManagerResult<DeviceRequirements> {
    const PIO_SPAN: usize = 0x0c;
    const DMA_OFFSET: u16 = 4;
    if size != PIO_SPAN {
        return Err(DeviceManagerError::InvalidConfig {
            operation: "declare x86 PIO fw_cfg device",
            detail: format!("fw_cfg PIO span must be {PIO_SPAN:#x} bytes"),
        });
    }
    let base = u16::try_from(base.as_usize()).map_err(fw_cfg_range_conversion_error)?;
    let dma_base =
        base.checked_add(DMA_OFFSET)
            .ok_or_else(|| DeviceManagerError::InvalidConfig {
                operation: "declare x86 PIO fw_cfg device",
                detail: "fw_cfg DMA port range overflows 16 bits".into(),
            })?;
    DeviceRequirements::new()
        .with_pio(
            ResourceSlot::new("selector-data")?,
            2,
            1,
            ResourceRequest::Fixed(base),
        )?
        .with_pio(
            ResourceSlot::new("dma")?,
            8,
            4,
            ResourceRequest::Fixed(dma_base),
        )
}

fn fw_cfg_range_conversion_error(_error: core::num::TryFromIntError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "build fw_cfg device",
        detail: "planned fw_cfg MMIO range exceeds the target address width".into(),
    }
}

impl FwCfgDeviceFactory {
    /// Creates the fw_cfg payload factory.
    pub const fn new() -> Self {
        Self
    }

    /// Builds the fw_cfg MMIO device and declares its guest-memory need.
    pub fn build(&self, config: FwCfgBuildConfig) -> DeviceManagerResult<DeviceBundle> {
        let fw_cfg = Arc::new(FwCfg::new(
            config.base,
            config.size,
            config.kernel,
            config.initrd,
            config.cmdline,
            config.cpu_num,
            config.platform,
        ));
        let dma_grant = DmaGrant::new();
        Ok(DeviceBundle::new().with_guest_memory_device_grant(
            Arc::new(FwCfgDmaDevice::from_arc(fw_cfg, dma_grant.clone())),
            dma_grant,
        ))
    }

    /// Builds the x86 PIO transport over the same fw_cfg selector state.
    pub fn build_pio(
        &self,
        selector_base: u16,
        selector_size: u16,
        dma_base: u16,
        dma_size: u16,
        config: FwCfgBuildConfig,
    ) -> DeviceManagerResult<DeviceBundle> {
        let fw_cfg = Arc::new(FwCfg::new(
            GuestPhysAddr::from_usize(0),
            FW_CFG_DMA_OFFSET + core::mem::size_of::<u64>(),
            config.kernel,
            config.initrd,
            config.cmdline,
            config.cpu_num,
            config.platform,
        ));
        let dma_grant = DmaGrant::new();
        Ok(DeviceBundle::new().with_guest_memory_device_grant(
            Arc::new(super::FwCfgPioDevice::new(
                fw_cfg,
                selector_base,
                selector_size,
                dma_base,
                dma_size,
                dma_grant.clone(),
            )?),
            dma_grant,
        ))
    }
}

impl Default for FwCfgDeviceFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl FwCfgDmaDevice {
    pub fn from_arc(inner: Arc<FwCfg>, dma_grant: DmaGrant) -> Self {
        let resource = inner.mmio_resource();
        Self {
            inner,
            dma_grant,
            name: String::from("fw-cfg"),
            resources: alloc::vec![resource].into_boxed_slice(),
        }
    }
}

impl Device for FwCfgDmaDevice {
    fn name(&self) -> &str {
        &self.name
    }
    fn resources(&self) -> &[Resource] {
        &self.resources
    }
    fn access(
        &self,
        access: &BusAccess,
        context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let addr = GuestPhysAddr::from_usize(access.addr as usize);
        if access.is_read {
            return self
                .inner
                .read_register(addr, access.width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                });
        }
        if !self.inner.is_dma_address(addr) {
            return self
                .inner
                .write_register(addr, access.width, access.data as usize)
                .map(|_| BusResponse::Write);
        }
        let Some(descriptor) = self
            .inner
            .write_dma_address(addr, access.width, access.data as usize)
            .map_err(DeviceError::from)?
        else {
            // A 32-bit write of the descriptor's high half only updates the
            // latch; the low-half write starts the DMA transaction.
            return Ok(BusResponse::Write);
        };
        let context = RefCell::new(context);
        self.inner
            .process_dma(
                descriptor,
                |gpa, data| {
                    context
                        .borrow_mut()
                        .read_guest_memory(&self.dma_grant, gpa, data)
                        .map_err(crate::DeviceManagerError::from)
                },
                |gpa, data| {
                    context
                        .borrow_mut()
                        .write_guest_memory(&self.dma_grant, gpa, data)
                        .map_err(crate::DeviceManagerError::from)
                },
            )
            .map_err(DeviceError::from)?;
        Ok(BusResponse::Write)
    }
}
