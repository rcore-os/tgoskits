#![allow(dead_code)]

mod definitions;
mod flags;
mod frame;
mod instructions;
mod percpu;
mod structs;
mod vcpu;
mod vmcb;

pub use percpu::SvmPerCpuState;
pub use vcpu::SvmVcpu;

pub fn has_hardware_support() -> bool {
    raw_cpuid::CpuId::new()
        .get_extended_processor_and_feature_identifiers()
        .map(|features| features.has_svm())
        .unwrap_or(false)
}
