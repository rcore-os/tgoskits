# x86_vcpu

[![CI](https://github.com/arceos-hypervisor/x86_vcpu/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/arceos-hypervisor/x86_vcpu/actions/workflows/ci.yml)

Definition of the vCPU structure and virtualization-related interface support for x86_64 architecture.

The crate user must implement the `AxVCpuHal` trait to provide the required low-level implementantion, 
relevant implementation can refer to [axvcpu](https://github.com/arceos-hypervisor/axvcpu/blob/main/src/hal.rs).
