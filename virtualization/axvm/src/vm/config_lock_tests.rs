use std::{
    string::String,
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Duration,
};

use ax_std::{os::arceos::sync::IrqSafeMutex, sync::Mutex as SleepMutex};

use super::*;

fn test_vm(id: VMId) -> AxVMRef {
    test_vm_with_machine(id, Machine::Destroyed)
}

fn test_vm_with_machine(
    id: VMId,
    machine: Machine<AxVMResources, Arc<VmRuntimeHandle>>,
) -> AxVMRef {
    let config = AxVMConfig::default_for_test(id, "config-lock-test");
    Arc::new(AxVM {
        id,
        name: config.name(),
        config: SleepMutex::new(config),
        machine: IrqSafeMutex::new(machine),
        fw_cfg_payload: Arc::new(FwCfgPayloadSlot::new()),
    })
}

#[test]
fn config_and_machine_use_their_required_lock_types() {
    fn assert_sleep_mutex<T: ?Sized>(_: &SleepMutex<T>) {}
    fn assert_irq_safe_mutex<T: ?Sized>(_: &IrqSafeMutex<T>) {}

    fn check(vm: &AxVM) {
        assert_sleep_mutex(&vm.config);
        assert_irq_safe_mutex(&vm.machine);
    }

    let _ = check as fn(&AxVM);
}

#[test]
fn with_config_reads_back_mutations() {
    let vm = test_vm(1);
    let dtb_load_gpa = GuestPhysAddr::from_usize(0x9000_0000);

    vm.with_config(|config| {
        config.set_dtb_load_gpa(dtb_load_gpa);
        config.exclude_device_path(String::from("/runtime-device"));
    });

    let (observed_dtb, has_runtime_device) = vm.with_config(|config| {
        (
            config.image_config().dtb_load_gpa,
            config
                .excluded_devices()
                .iter()
                .flatten()
                .any(|path| path == "/runtime-device"),
        )
    });
    assert_eq!(observed_dtb, Some(dtb_load_gpa));
    assert!(has_runtime_device);
}

#[test]
fn with_config_serializes_concurrent_mutations() {
    const WRITER_COUNT: usize = 4;

    let vm = test_vm(2);
    let start = Arc::new(Barrier::new(WRITER_COUNT + 1));
    let writers = (0..WRITER_COUNT)
        .map(|writer_id| {
            let vm = vm.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                vm.with_config(|config| {
                    config.exclude_device_path(format!("/concurrent-device-{writer_id}"));
                });
            })
        })
        .collect::<Vec<_>>();

    start.wait();
    for writer in writers {
        writer.join().expect("concurrent config writer panicked");
    }

    vm.with_config(|config| {
        let excluded = config.excluded_devices();
        for writer_id in 0..WRITER_COUNT {
            let expected = format!("/concurrent-device-{writer_id}");
            assert!(
                excluded.iter().flatten().any(|path| path == &expected),
                "missing mutation from writer {writer_id}"
            );
        }
    });
}

#[test]
fn with_config_mutation_does_not_wait_for_machine_lock() {
    let vm = test_vm(3);
    let (machine_locked_tx, machine_locked_rx) = mpsc::channel();
    let (release_machine_tx, release_machine_rx) = mpsc::channel();

    let machine_vm = vm.clone();
    let machine_thread = thread::spawn(move || {
        let _machine = machine_vm.machine.lock();
        machine_locked_tx
            .send(())
            .expect("announce held machine lock");
        release_machine_rx
            .recv()
            .expect("wait for machine lock release");
    });
    machine_locked_rx
        .recv()
        .expect("wait until machine lock is held");

    let (config_started_tx, config_started_rx) = mpsc::channel();
    let (config_done_tx, config_done_rx) = mpsc::channel();
    let config_vm = vm.clone();
    let config_thread = thread::spawn(move || {
        config_started_tx.send(()).expect("announce config access");
        config_vm.with_config(|config| {
            config.exclude_device_path(String::from("/independent-config-write"));
        });
        config_done_tx.send(()).expect("announce config completion");
    });
    config_started_rx.recv().expect("wait for config access");

    let config_result = config_done_rx.recv_timeout(Duration::from_secs(2));

    release_machine_tx
        .send(())
        .expect("release held machine lock");
    machine_thread.join().expect("machine lock holder panicked");
    config_thread.join().expect("config writer panicked");

    assert!(
        config_result.is_ok(),
        "with_config waited for the independent machine lock"
    );
}

#[test]
fn with_config_remains_available_without_machine_resources() {
    let states = [
        Machine::Destroying,
        Machine::Destroyed,
        Machine::Failed(String::from("test failure")),
    ];

    for (index, machine) in states.into_iter().enumerate() {
        let vm = test_vm_with_machine(index + 4, machine);
        let expected_status = vm.status();
        let dtb_load_gpa = GuestPhysAddr::from_usize(0xa000_0000 + index * 0x1000);

        vm.with_config(|config| config.set_dtb_load_gpa(dtb_load_gpa));

        assert_eq!(vm.status(), expected_status);
        assert_eq!(
            vm.with_config(|config| config.image_config().dtb_load_gpa),
            Some(dtb_load_gpa)
        );
    }
}

#[test]
fn runtime_handle_returns_without_machine_lock() {
    let runtime = Arc::new(VmRuntimeHandle::new());
    let vm = test_vm_with_machine(
        7,
        Machine::Stopping {
            resources: None,
            runtime: Some(runtime),
            reason: StopReason::Clean,
        },
    );

    let runtime = vm.runtime_handle().unwrap();
    assert!(
        vm.machine.try_lock().is_some(),
        "runtime handle access must not retain the machine lock"
    );
    runtime.notify_all();
}
