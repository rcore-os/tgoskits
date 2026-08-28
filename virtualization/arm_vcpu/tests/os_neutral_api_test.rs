// Copyright 2026 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![cfg(target_arch = "aarch64")]

use core::{
    marker::PhantomData,
    sync::atomic::{AtomicUsize, Ordering},
};

use arm_vcpu::{
    ArmHostOps, ArmHostPageFaultAccess, ArmPerCpu, ArmVcpu, ArmVcpuCreateConfig, ArmVcpuResult,
};

struct DummyHost;
struct BorrowedHost<'a>(PhantomData<&'a mut ()>);

static INJECTED_INTERRUPT: AtomicUsize = AtomicUsize::new(usize::MAX);

impl ArmHostOps for DummyHost {
    fn inject_virtual_interrupt(vector: u32) -> ArmVcpuResult {
        INJECTED_INTERRUPT.store(vector as usize, Ordering::Relaxed);
        Ok(())
    }

    fn finish_pending_host_irq(_raw_ack: u32) -> Option<usize> {
        None
    }

    fn handle_current_host_irq() {}
}

impl ArmHostOps for BorrowedHost<'_> {
    fn inject_virtual_interrupt(_vector: u32) -> ArmVcpuResult {
        Ok(())
    }

    fn finish_pending_host_irq(_raw_ack: u32) -> Option<usize> {
        None
    }

    fn handle_current_host_irq() {}
}

#[test]
fn virtual_interrupt_id_preserves_full_gic_intid() {
    const HOST_UART_INTID: usize = 365;

    INJECTED_INTERRUPT.store(usize::MAX, Ordering::Relaxed);
    let mut vcpu = ArmVcpu::<DummyHost>::new(1, 0, ArmVcpuCreateConfig::default()).unwrap();
    vcpu.inject_interrupt(HOST_UART_INTID).unwrap();

    assert_eq!(INJECTED_INTERRUPT.load(Ordering::Relaxed), HOST_UART_INTID);
}

#[test]
fn default_host_page_fault_callback_is_source_compatible_and_unhandled() {
    let mut saved_pc = 0x8020_0000;

    assert!(!DummyHost::handle_current_host_page_fault(
        &mut saved_pc,
        0xffff_0000,
        ArmHostPageFaultAccess::Write,
        true,
    ));
    assert_eq!(saved_pc, 0x8020_0000);
}

#[test]
fn host_ops_and_generic_apis_accept_a_non_static_implementor() {
    fn compile_with_borrowed_host<'a>(_borrow: &'a mut ()) {
        ArmVcpu::<BorrowedHost<'a>>::new(1, 0, ArmVcpuCreateConfig::default()).unwrap();
        let _hardware_enable: fn(&mut ArmPerCpu) -> ArmVcpuResult =
            ArmPerCpu::hardware_enable::<BorrowedHost<'a>>;
    }

    let mut borrowed = ();
    compile_with_borrowed_host(&mut borrowed);
}
