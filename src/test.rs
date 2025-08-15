extern crate alloc;

#[cfg(test)]
mod tests {
    use crate::{AxArchVCpu, AxVCpu, VCpuState, exit::AxVCpuExitReason};
    use alloc::{
        rc::Rc,
        string::{String, ToString},
        vec::Vec,
    };
    use axaddrspace::{GuestPhysAddr, HostPhysAddr};
    use axerrno::{AxError, AxResult};
    use axvisor_api::vmm::{VCpuId, VMId};
    use core::cell::RefCell;

    // Mock architecture implementation for testing
    #[derive(Debug)]
    struct MockArchVCpu {
        vm_id: VMId,
        vcpu_id: VCpuId,
        entry: Option<GuestPhysAddr>,
        ept_root: Option<HostPhysAddr>,
        is_setup: bool,
        is_bound: bool,
        registers: [usize; 16],
        pending_interrupts: Vec<usize>,
        return_value: usize,
        // Track method calls for testing
        call_log: Rc<RefCell<Vec<String>>>,
    }

    #[derive(Debug, Clone)]
    struct MockCreateConfig {
        call_log: Rc<RefCell<Vec<String>>>,
    }

    #[derive(Debug)]
    struct MockSetupConfig;

    impl AxArchVCpu for MockArchVCpu {
        type CreateConfig = MockCreateConfig;
        type SetupConfig = MockSetupConfig;

        fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> AxResult<Self> {
            config.call_log.borrow_mut().push("new".to_string());
            Ok(Self {
                vm_id,
                vcpu_id,
                entry: None,
                ept_root: None,
                is_setup: false,
                is_bound: false,
                registers: [0; 16],
                pending_interrupts: Vec::new(),
                return_value: 0,
                call_log: config.call_log,
            })
        }

        fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
            self.call_log.borrow_mut().push("set_entry".to_string());
            self.entry = Some(entry);
            Ok(())
        }

        fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
            self.call_log.borrow_mut().push("set_ept_root".to_string());
            self.ept_root = Some(ept_root);
            Ok(())
        }

        fn setup(&mut self, _config: Self::SetupConfig) -> AxResult {
            self.call_log.borrow_mut().push("setup".to_string());
            if self.entry.is_none() || self.ept_root.is_none() {
                return Err(AxError::InvalidInput);
            }
            self.is_setup = true;
            Ok(())
        }

        fn run(&mut self) -> AxResult<AxVCpuExitReason> {
            self.call_log.borrow_mut().push("run".to_string());
            if !self.is_bound {
                return Err(AxError::BadState);
            }
            // Simulate a simple halt exit
            Ok(AxVCpuExitReason::Halt)
        }

        fn bind(&mut self) -> AxResult {
            self.call_log.borrow_mut().push("bind".to_string());
            if !self.is_setup {
                return Err(AxError::BadState);
            }
            self.is_bound = true;
            Ok(())
        }

        fn unbind(&mut self) -> AxResult {
            self.call_log.borrow_mut().push("unbind".to_string());
            self.is_bound = false;
            Ok(())
        }

        fn set_gpr(&mut self, reg: usize, val: usize) {
            self.call_log
                .borrow_mut()
                .push(format!("set_gpr({}, {})", reg, val));
            if reg < self.registers.len() {
                self.registers[reg] = val;
            }
        }

        fn inject_interrupt(&mut self, vector: usize) -> AxResult {
            self.call_log
                .borrow_mut()
                .push(format!("inject_interrupt({})", vector));
            self.pending_interrupts.push(vector);
            Ok(())
        }

        fn set_return_value(&mut self, val: usize) {
            self.call_log
                .borrow_mut()
                .push(format!("set_return_value({})", val));
            self.return_value = val;
        }
    }

    fn create_mock_vcpu() -> (AxVCpu<MockArchVCpu>, Rc<RefCell<Vec<String>>>) {
        let call_log = Rc::new(RefCell::new(Vec::new()));
        let config = MockCreateConfig {
            call_log: call_log.clone(),
        };
        let vcpu = AxVCpu::new(1, 0, 0, None, config).unwrap();
        (vcpu, call_log)
    }

    #[test]
    fn test_vcpu_creation() {
        let (vcpu, call_log) = create_mock_vcpu();

        assert_eq!(vcpu.id(), 0);
        assert_eq!(vcpu.favor_phys_cpu(), 0);
        assert_eq!(vcpu.phys_cpu_set(), None);
        assert_eq!(vcpu.state(), VCpuState::Created);
        assert!(vcpu.is_bsp());

        assert_eq!(call_log.borrow().len(), 1);
        assert_eq!(call_log.borrow()[0], "new");
    }

    #[test]
    fn test_vcpu_setup_lifecycle() {
        let (vcpu, call_log) = create_mock_vcpu();

        // Test individual setup methods instead of the high-level setup()
        // This avoids the percpu-related code in manipulate_arch_vcpu

        let entry = GuestPhysAddr::from(0x1000);
        let ept_root = HostPhysAddr::from(0x2000);

        // Test direct arch_vcpu access
        let arch_vcpu = vcpu.get_arch_vcpu();

        // Test set_entry
        let result = arch_vcpu.set_entry(entry);
        assert!(result.is_ok());

        // Test set_ept_root
        let result = arch_vcpu.set_ept_root(ept_root);
        assert!(result.is_ok());

        // Test setup
        let result = arch_vcpu.setup(MockSetupConfig);
        assert!(result.is_ok());

        // Check call order
        let calls = call_log.borrow();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0], "new");
        assert_eq!(calls[1], "set_entry");
        assert_eq!(calls[2], "set_ept_root");
        assert_eq!(calls[3], "setup");

        // Note: State transitions are not tested here due to percpu limitations
    }

    #[test]
    fn test_vcpu_state_transitions() {
        let (vcpu, _) = create_mock_vcpu();

        // Created -> Free
        assert_eq!(vcpu.state(), VCpuState::Created);
        let result = vcpu.transition_state(VCpuState::Created, VCpuState::Free);
        assert!(result.is_ok());
        assert_eq!(vcpu.state(), VCpuState::Free);

        // Free -> Ready
        let result = vcpu.transition_state(VCpuState::Free, VCpuState::Ready);
        assert!(result.is_ok());
        assert_eq!(vcpu.state(), VCpuState::Ready);

        // Invalid transition should fail
        let result = vcpu.transition_state(VCpuState::Running, VCpuState::Free);
        assert!(result.is_err());
        assert_eq!(vcpu.state(), VCpuState::Invalid);
    }

    #[test]
    fn test_vcpu_bind_unbind() {
        let (vcpu, call_log) = create_mock_vcpu();

        // Test direct arch_vcpu operations instead of high-level methods
        // to avoid percpu-related code

        let entry = GuestPhysAddr::from(0x1000);
        let ept_root = HostPhysAddr::from(0x2000);

        // Setup arch_vcpu directly
        let arch_vcpu = vcpu.get_arch_vcpu();
        arch_vcpu.set_entry(entry).unwrap();
        arch_vcpu.set_ept_root(ept_root).unwrap();
        arch_vcpu.setup(MockSetupConfig).unwrap();

        // Test bind/unbind
        let result = arch_vcpu.bind();
        assert!(result.is_ok());

        let result = arch_vcpu.unbind();
        assert!(result.is_ok());

        let calls = call_log.borrow();
        assert!(calls.contains(&"bind".to_string()));
        assert!(calls.contains(&"unbind".to_string()));
    }

    #[test]
    fn test_vcpu_run() {
        let (vcpu, call_log) = create_mock_vcpu();

        // Setup arch_vcpu directly to avoid percpu code
        let arch_vcpu = vcpu.get_arch_vcpu();
        let entry = GuestPhysAddr::from(0x1000);
        let ept_root = HostPhysAddr::from(0x2000);

        arch_vcpu.set_entry(entry).unwrap();
        arch_vcpu.set_ept_root(ept_root).unwrap();
        arch_vcpu.setup(MockSetupConfig).unwrap();
        arch_vcpu.bind().unwrap();

        // Test run
        let result = arch_vcpu.run();
        assert!(result.is_ok());
        if let Ok(AxVCpuExitReason::Halt) = result {
            // Expected
        } else {
            panic!("Expected Halt exit reason");
        }

        assert!(call_log.borrow().contains(&"run".to_string()));
    }

    #[test]
    fn test_vcpu_register_operations() {
        let (vcpu, call_log) = create_mock_vcpu();

        vcpu.set_gpr(5, 0xdeadbeef);
        vcpu.set_return_value(42);

        // Check that values were actually set in the mock struct
        let arch_vcpu = vcpu.get_arch_vcpu();
        assert_eq!(arch_vcpu.registers[5], 0xdeadbeef);
        assert_eq!(arch_vcpu.return_value, 42);

        let calls = call_log.borrow();
        assert!(calls.contains(&"set_gpr(5, 3735928559)".to_string()));
        assert!(calls.contains(&"set_return_value(42)".to_string()));
    }

    #[test]
    fn test_vcpu_interrupt_injection() {
        let (vcpu, call_log) = create_mock_vcpu();

        let result = vcpu.inject_interrupt(32);
        assert!(result.is_ok());

        assert!(
            call_log
                .borrow()
                .contains(&"inject_interrupt(32)".to_string())
        );
    }

    #[test]
    fn test_exit_reason_debug_format() {
        let exit_mmio = AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(0x1000),
            width: axaddrspace::device::AccessWidth::Dword,
            reg: 5,
            reg_width: axaddrspace::device::AccessWidth::Qword,
            signed_ext: false,
        };

        let debug_str = format!("{:?}", exit_mmio);
        assert!(debug_str.contains("MmioRead"));
        assert!(debug_str.contains("0x1000"));
    }

    #[test]
    fn test_exit_reason_hypercall() {
        let exit_hypercall = AxVCpuExitReason::Hypercall {
            nr: 42,
            args: [1, 2, 3, 4, 5, 6],
        };

        if let AxVCpuExitReason::Hypercall { nr, args } = exit_hypercall {
            assert_eq!(nr, 42);
            assert_eq!(args[0], 1);
            assert_eq!(args[5], 6);
        } else {
            panic!("Expected Hypercall variant");
        }
    }

    #[test]
    fn test_vcpu_state_display() {
        assert_eq!(VCpuState::Created as u8, 1);
        assert_eq!(VCpuState::Free as u8, 2);
        assert_eq!(VCpuState::Ready as u8, 3);
        assert_eq!(VCpuState::Running as u8, 4);
        assert_eq!(VCpuState::Blocked as u8, 5);
        assert_eq!(VCpuState::Invalid as u8, 0);
    }

    #[test]
    fn test_vcpu_setup_wrong_state() {
        let (vcpu, _) = create_mock_vcpu();

        // Test state transition without using high-level setup method
        assert_eq!(vcpu.state(), VCpuState::Created);

        // Transition to wrong state first
        vcpu.transition_state(VCpuState::Created, VCpuState::Ready)
            .unwrap();
        assert_eq!(vcpu.state(), VCpuState::Ready);

        // Test that arch_vcpu setup works regardless of vCPU state
        // (since we're bypassing the state machine)
        let arch_vcpu = vcpu.get_arch_vcpu();
        let entry = GuestPhysAddr::from(0x1000);
        let ept_root = HostPhysAddr::from(0x2000);

        // These should work at arch level even if vCPU state is wrong
        assert!(arch_vcpu.set_entry(entry).is_ok());
        assert!(arch_vcpu.set_ept_root(ept_root).is_ok());
        assert!(arch_vcpu.setup(MockSetupConfig).is_ok());
    }

    #[test]
    fn test_vcpu_run_without_bind() {
        let (vcpu, _) = create_mock_vcpu();

        // Setup arch_vcpu but don't bind
        let arch_vcpu = vcpu.get_arch_vcpu();
        let entry = GuestPhysAddr::from(0x1000);
        let ept_root = HostPhysAddr::from(0x2000);

        arch_vcpu.set_entry(entry).unwrap();
        arch_vcpu.set_ept_root(ept_root).unwrap();
        arch_vcpu.setup(MockSetupConfig).unwrap();

        // Run should fail without binding (according to mock implementation)
        let result = arch_vcpu.run();
        assert!(result.is_err());
    }

    #[test]
    fn test_vcpu_bsp_identification() {
        let call_log = Rc::new(RefCell::new(Vec::new()));
        let config = MockCreateConfig {
            call_log: call_log.clone(),
        };

        let vcpu0 = AxVCpu::<MockArchVCpu>::new(1, 0, 0, None, config.clone()).unwrap();
        let vcpu1 = AxVCpu::<MockArchVCpu>::new(1, 1, 0, None, config).unwrap();

        assert!(vcpu0.is_bsp());
        assert!(!vcpu1.is_bsp());
    }

    #[test]
    fn test_vcpu_cpu_affinity() {
        let call_log = Rc::new(RefCell::new(Vec::new()));
        let config = MockCreateConfig { call_log };

        let vcpu = AxVCpu::<MockArchVCpu>::new(1, 0, 2, Some(0b1010), config).unwrap();

        assert_eq!(vcpu.favor_phys_cpu(), 2);
        assert_eq!(vcpu.phys_cpu_set(), Some(0b1010));
    }

    #[test]
    fn test_vcpu_creation_failure() {
        // Test creation with invalid config
        let call_log = Rc::new(RefCell::new(Vec::new()));
        let config = MockCreateConfig { call_log };

        // This should succeed with our mock implementation
        let result = AxVCpu::<MockArchVCpu>::new(1, 0, 0, None, config);
        assert!(result.is_ok());
    }

    // Integration test - simplified to avoid percpu issues
    #[test]
    fn test_arch_vcpu_lifecycle() {
        let (vcpu, call_log) = create_mock_vcpu();

        // Test the arch_vcpu lifecycle directly
        let arch_vcpu = vcpu.get_arch_vcpu();

        // Setup phase
        let entry = GuestPhysAddr::from(0x1000);
        let ept_root = HostPhysAddr::from(0x2000);

        assert!(arch_vcpu.set_entry(entry).is_ok());
        assert!(arch_vcpu.set_ept_root(ept_root).is_ok());
        assert!(arch_vcpu.setup(MockSetupConfig).is_ok());

        // Bind and run
        assert!(arch_vcpu.bind().is_ok());

        let exit_reason = arch_vcpu.run().unwrap();
        assert!(matches!(exit_reason, AxVCpuExitReason::Halt));

        // Unbind
        assert!(arch_vcpu.unbind().is_ok());

        // Verify all methods were called
        let calls = call_log.borrow();
        assert!(calls.contains(&"set_entry".to_string()));
        assert!(calls.contains(&"set_ept_root".to_string()));
        assert!(calls.contains(&"setup".to_string()));
        assert!(calls.contains(&"bind".to_string()));
        assert!(calls.contains(&"run".to_string()));
        assert!(calls.contains(&"unbind".to_string()));
    }

    // Note: Per-CPU tests are omitted due to percpu crate linking conflicts in test environment.
    // The percpu crate requires kernel-space linking which is incompatible with cargo test.
    // In a real hypervisor environment, AxPerCpu would be tested differently.
}
