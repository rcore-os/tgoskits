use std::sync::atomic::{AtomicUsize, Ordering};

use acpica_interpreter::{
    ALREADY_EXISTS, BAD_PARAMETER, Engine, LIMIT, Mode, SUPPORT, Value, aml_error_count,
    host::OfflineBackend,
};

unsafe extern "C" {
    fn acpica_interpreter_test_ec_access(
        function: u32,
        address: u64,
        width: u32,
        value: *mut u64,
        context: *mut std::ffi::c_void,
    ) -> u32;
    fn acpica_interpreter_table_references(index: u32) -> u32;
    fn acpica_interpreter_table(index: u32, out: *mut u8, capacity: usize, used: *mut usize)
    -> u32;
}

static NOTIFIED: AtomicUsize = AtomicUsize::new(0);

fn notification(path: &str, value: u32) {
    assert_eq!(path, "\\_SB_.PWRB");
    NOTIFIED.store(value as usize, Ordering::Release);
}

#[test]
fn production_interpreter_loads_and_evaluates_authored_aml() {
    let data = include_bytes!("synthetic.aml").to_vec().into_boxed_slice();
    let backend = OfflineBackend::from_tables(vec![data]).unwrap().register();

    // A caller-preparation failure must terminate ACPICA and release the
    // lifecycle claim so this backend can retry initialization safely.
    // SAFETY: the fixture and backend simulate every ACPICA hardware access.
    let failed =
        unsafe { Engine::initialize_with_tables(backend, Mode::Offline, |_| Err(BAD_PARAMETER)) };
    assert!(matches!(failed, Err(BAD_PARAMETER)));

    // SAFETY: the fixture is authored and OfflineBackend simulates every access.
    let mut engine = unsafe { Engine::initialize(backend, Mode::Offline) }.unwrap();

    let mut ec_value = 0;
    // SAFETY: the host-only C shim forwards through the production EC bridge
    // using a valid output pointer and a null, unused context.
    assert_eq!(
        unsafe { acpica_interpreter_test_ec_access(0, 3, 8, &mut ec_value, core::ptr::null_mut()) },
        0
    );
    assert_eq!(ec_value, 0x42);
    // The OSL must reject invalid directions and unsupported access widths.
    assert_eq!(
        unsafe { acpica_interpreter_test_ec_access(2, 3, 8, &mut ec_value, core::ptr::null_mut()) },
        BAD_PARAMETER
    );
    assert_eq!(
        unsafe {
            acpica_interpreter_test_ec_access(0, 3, 16, &mut ec_value, core::ptr::null_mut())
        },
        SUPPORT
    );

    let count = engine.table_count().unwrap();
    assert!(count > 0);
    for index in 0..count {
        // SAFETY: the live offline engine serializes table-reference access.
        let before = unsafe { acpica_interpreter_table_references(index) };
        assert_ne!(before, u32::MAX);
        for _ in 0..8 {
            assert!(!engine.table(index).unwrap().is_empty());
            let mut byte = 0;
            let mut used = usize::MAX;
            // SAFETY: one-byte output storage is writable for the duration of the call.
            assert_eq!(
                unsafe { acpica_interpreter_table(index, &mut byte, 1, &mut used) },
                LIMIT
            );
            assert_eq!(used, 0);
            // SAFETY: the same table lock protects the test-only reference observer.
            assert_eq!(
                unsafe { acpica_interpreter_table_references(index) },
                before
            );
        }
    }

    engine.install_notify(notification).unwrap();
    engine.initialize_objects().unwrap();
    assert_eq!(
        engine.evaluate("\\_S5", &[]).unwrap(),
        Value::Package(vec![Value::Integer(5), Value::Integer(5)])
    );
    assert_eq!(engine.integer("\\_SB.PWRB._STA").unwrap(), 15);
    assert!(
        engine
            .namespace()
            .unwrap()
            .iter()
            .any(|node| node.path == "\\_SB_.PWRB" && node.kind == 6)
    );
    assert_eq!(engine.integer("\\_SB.PWRB.ICNT").unwrap(), 1);
    let repeated_initialization = engine.initialize_objects();
    let initialization_count = engine.integer("\\_SB.PWRB.ICNT").unwrap();
    assert_eq!(
        (repeated_initialization, initialization_count),
        (Err(ALREADY_EXISTS), 1)
    );
    assert!(matches!(
        engine.evaluate("\\_SB.PCI0._PRT", &[]),
        Ok(Value::Package(_))
    ));
    engine.evaluate("\\_SB.PWRB.TEST", &[]).unwrap();
    backend.0.wait_events();
    assert_eq!(NOTIFIED.load(Ordering::Acquire), 0x80);
    assert!(engine.evaluate("\\DOES_NOT_EXIST", &[]).is_err());
    drop(engine);
    assert_eq!(aml_error_count(), 0);
}
