use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use axerrno::{ax_err, AxResult};

use crate::config::AxVMConfig;
use crate::AxVCpu;
use crate::{has_hardware_support, AxVMHal, HostPhysAddr};

struct AxVMInnerConst<H: AxVMHal> {
    id: usize,
    vcpu_list: Box<[AxVCpu<H>]>,
    // to be added: device_list: ...
}

struct AxVMInnerMut<H: AxVMHal> {
    // memory: ...
    _marker: core::marker::PhantomData<H>,
}

/// A Virtual Machine.
pub struct AxVM<H: AxVMHal> {
    inner_const: AxVMInnerConst<H>,
    inner_mut: AxVMInnerMut<H>,
}

impl<H: AxVMHal> AxVM<H> {
    pub fn new(config: AxVMConfig, id: usize) -> AxResult<Arc<Self>> {
        let result = Arc::new_cyclic(|weak_self| {
            let mut vcpu_list = Vec::with_capacity(config.cpu_count);
            for vcpu_id in 0..config.cpu_count {
                vcpu_list.push(
                    AxVCpu::new(config.cpu_config, vcpu_id, weak_self.clone(), 0, 0).unwrap(),
                );
            }

            Self {
                inner_const: AxVMInnerConst {
                    id,
                    vcpu_list: vcpu_list.into_boxed_slice(),
                },
                inner_mut: AxVMInnerMut {
                    _marker: core::marker::PhantomData,
                },
            }
        });

        result.init_vcpu()?;

        Ok(result)
    }

    #[inline]
    pub fn id(&self) -> usize {
        self.inner_const.id
    }

    #[inline]
    pub fn vcpu(&self, vcpu_id: usize) -> Option<&AxVCpu<H>> {
        self.vcpu_list().get(vcpu_id)
    }

    #[inline]
    pub fn vcpu_list(&self) -> &[AxVCpu<H>] {
        &self.inner_const.vcpu_list
    }

    #[inline]
    fn init_vcpu(&self) -> AxResult {
        for vcpu in self.vcpu_list() {
            vcpu.init()?;
        }

        Ok(())
    }

    pub fn ept_root(&self) -> HostPhysAddr {
        unimplemented!()
    }

    pub fn boot(&self) -> AxResult {
        if !has_hardware_support() {
            ax_err!(Unsupported, "Hardware does not support virtualization")
        } else {
            self.run_vcpu(0)
        }
    }

    pub fn run_vcpu(&self, vcpu_id: usize) -> AxResult {
        loop {
            // todo: device access
            let _ = self.vcpu(vcpu_id).unwrap().run()?;
        }
    }
}
