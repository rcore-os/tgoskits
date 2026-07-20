use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_std::os::arceos::task::WakeResult;

use super::{
    activation::{
        IoApicForwardingEnablePublication, activate_ready_ioapic_forwarding_batch_for_test,
        activate_ready_ioapic_forwarding_route_for_test, publish_ioapic_forwarding_owner,
        register_test_ioapic_forwarding_action, restore_ioapic_forwarding_enable_publication,
        revoke_ioapic_forwarding_routes,
    },
    handler::{
        forwarded_irq_return_after_wake, publish_forwarded_ioapic_irq_fact,
        should_rearm_forwarded_host_gsi_after_eoi,
    },
    state::*,
};
use crate::InterruptTriggerMode;

static ROUTE_TEST_LOCK: Mutex<()> = Mutex::new(());
static ACTIVATION_COUNT: AtomicUsize = AtomicUsize::new(0);
static TEST_DEVICE_ENDPOINT_UNMASKED: AtomicBool = AtomicBool::new(false);
static TEST_REVOKED_GSIS: AtomicUsize = AtomicUsize::new(0);

#[test]
fn pit_gsi_uses_synthetic_injection_not_host_irq_hook() {
    assert!(!should_register_ioapic_gsi_hook(PIT_TIMER_GSI));
}

#[test]
fn passthrough_gsis_still_register_host_irq_hooks() {
    assert!(should_register_ioapic_gsi_hook(COM1_GSI));
    assert!(should_register_ioapic_gsi_hook(18));
    assert!(should_register_ioapic_gsi_hook(IOAPIC_GSI_COUNT - 1));
    assert!(!should_register_ioapic_gsi_hook(IOAPIC_GSI_COUNT));
}

#[test]
fn hook_gsi_iterator_matches_registration_policy() {
    for gsi in 0..=IOAPIC_GSI_COUNT {
        assert_eq!(
            ioapic_irq_hook_gsis().any(|hook| hook == gsi),
            should_register_ioapic_gsi_hook(gsi)
        );
    }
}

#[test]
fn forwarded_gsi_bits_are_stable() {
    assert_eq!(gsi_bit(0), 1);
    assert_eq!(gsi_bit(18), 1usize << 18);
}

#[test]
fn host_irq_storage_preserves_domain_and_hwirq() {
    let irq = crate::arch::x86_64::host_irq::make_irq_id(2, 18);
    assert_eq!(raw_to_host_irq(host_irq_to_raw(irq)), irq);
}

#[test]
fn explicit_forwarding_route_wins_over_fallback_route() {
    with_clean_forwarding_routes(|| {
        let fallback_guest_gsi = 7;
        let explicit_guest_gsi = 18;
        let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 7);
        IOAPIC_HOST_IRQS[fallback_guest_gsi].store(host_irq_to_raw(host_irq), Ordering::Release);

        register_ioapic_irq_forwarding_route(explicit_guest_gsi, host_irq).unwrap();

        assert_eq!(guest_gsi_for_host_irq(host_irq), Some(explicit_guest_gsi));
    });
}

#[test]
fn fallback_registration_skips_host_irq_owned_by_explicit_route() {
    with_clean_forwarding_routes(|| {
        let fallback_guest_gsi = 10;
        let explicit_guest_gsi = 18;
        let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
        IOAPIC_HOST_IRQS[fallback_guest_gsi].store(host_irq_to_raw(host_irq), Ordering::Release);

        register_ioapic_irq_forwarding_route(explicit_guest_gsi, host_irq).unwrap();

        assert!(host_irq_has_explicit_route_for_other_gsi(
            host_irq,
            fallback_guest_gsi
        ));
        assert!(!host_irq_has_explicit_route_for_other_gsi(
            host_irq,
            explicit_guest_gsi
        ));
    });
}

#[test]
fn exclusive_forwarding_action_rejects_an_aliased_guest_route() {
    with_clean_forwarding_routes(|| {
        let first_guest_gsi = 17;
        let second_guest_gsi = 18;
        let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
        register_ioapic_irq_forwarding_route(first_guest_gsi, host_irq).unwrap();
        super::validate_ioapic_irq_forwarding_source(first_guest_gsi).unwrap();
        register_ioapic_irq_forwarding_route(second_guest_gsi, host_irq).unwrap();

        let error = super::validate_ioapic_irq_forwarding_source(second_guest_gsi)
            .expect_err("one host action cannot be shared by two guest routes");

        assert!(matches!(error, crate::AxVmError::ResourceConflict { .. }));
        assert!(IOAPIC_IRQ_HANDLES[second_guest_gsi].lock().is_none());
    });
}

#[test]
fn forwarding_trigger_mode_comes_from_registered_route_not_gsi_number() {
    with_clean_forwarding_routes(|| {
        let low_level_gsi = COM1_GSI;
        let high_edge_gsi = 18;
        let low_host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, low_level_gsi as u32);
        let high_host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, high_edge_gsi as u32);

        register_ioapic_irq_forwarding_route_with_trigger(
            low_level_gsi,
            low_host_irq,
            InterruptTriggerMode::LevelTriggered,
        )
        .unwrap();
        register_ioapic_irq_forwarding_route_with_trigger(
            high_edge_gsi,
            high_host_irq,
            InterruptTriggerMode::EdgeTriggered,
        )
        .unwrap();

        assert!(is_level_triggered_forwarded_host_gsi(low_level_gsi));
        assert!(!is_level_triggered_forwarded_host_gsi(high_edge_gsi));
    });
}

#[test]
fn forwarding_activation_waits_for_guest_route_and_runs_once() {
    with_clean_forwarding_routes(|| {
        let guest_gsi = 18;
        let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
        ACTIVATION_COUNT.store(0, Ordering::Release);
        register_ioapic_irq_forwarding_route(guest_gsi, host_irq).unwrap();
        register_test_ioapic_forwarding_action(guest_gsi).unwrap();
        super::register_ioapic_irq_forwarding_activation(
            guest_gsi,
            test_operations(count_activation),
        )
        .unwrap();

        activate_ready_ioapic_forwarding_route_for_test(guest_gsi, false).unwrap();
        assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 0);
        assert!(!crate::arch::x86_64::host_irq::test_irq_is_enabled(
            host_irq
        ));

        activate_ready_ioapic_forwarding_route_for_test(guest_gsi, true).unwrap();
        assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 1);

        activate_ready_ioapic_forwarding_route_for_test(guest_gsi, true).unwrap();
        assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 1);
    });
}

#[test]
fn forwarding_activation_requires_a_retained_action_handle() {
    with_clean_forwarding_routes(|| {
        let guest_gsi = 18;
        let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
        register_ioapic_irq_forwarding_route(guest_gsi, host_irq).unwrap();
        super::register_ioapic_irq_forwarding_activation(
            guest_gsi,
            test_operations(count_activation),
        )
        .unwrap();

        let error = activate_ready_ioapic_forwarding_route_for_test(guest_gsi, true)
            .expect_err("activation without the generation-owned action must fail closed");

        assert!(matches!(error, crate::AxVmError::Interrupt { .. }));
        assert!(matches!(
            *IOAPIC_FORWARDING_ROUTES[guest_gsi].lock(),
            IoApicForwardingRouteState::Prepared(_)
        ));
        assert_eq!(
            IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire) & gsi_bit(guest_gsi),
            0
        );
    });
}

#[test]
fn forwarding_activation_drops_pre_activation_pending_state() {
    with_clean_forwarding_routes(|| {
        let guest_gsi = 18;
        let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
        ACTIVATION_COUNT.store(0, Ordering::Release);
        register_ioapic_irq_forwarding_route(guest_gsi, host_irq).unwrap();
        register_test_ioapic_forwarding_action(guest_gsi).unwrap();
        super::register_ioapic_irq_forwarding_activation(
            guest_gsi,
            test_operations(count_activation),
        )
        .unwrap();
        mark_forwarded_ioapic_gsi_state(guest_gsi);

        activate_ready_ioapic_forwarding_route_for_test(guest_gsi, true).unwrap();

        assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 1);
        assert_eq!(forwarded_ioapic_gsi_state(guest_gsi), (false, false, false));
        assert!(crate::arch::x86_64::host_irq::test_irq_is_enabled(host_irq));
    });
}

#[test]
fn failed_forwarding_activation_remains_prepared_and_masked() {
    with_clean_forwarding_routes(|| {
        let guest_gsi = 18;
        let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
        ACTIVATION_COUNT.store(0, Ordering::Release);
        register_ioapic_irq_forwarding_route(guest_gsi, host_irq).unwrap();
        register_test_ioapic_forwarding_action(guest_gsi).unwrap();
        super::register_ioapic_irq_forwarding_activation(
            guest_gsi,
            test_operations(fail_activation),
        )
        .unwrap();
        mark_forwarded_ioapic_gsi_state(guest_gsi);

        let error = activate_ready_ioapic_forwarding_route_for_test(guest_gsi, true)
            .expect_err("endpoint activation failure must propagate");

        assert!(matches!(error, crate::AxVmError::Interrupt { .. }));
        assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 1);
        assert_eq!(
            forwarded_ioapic_gsi_state(guest_gsi),
            (false, false, true),
            "stale publications are dropped but the host line remains masked"
        );
        assert_eq!(
            IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire) & gsi_bit(guest_gsi),
            0,
            "failed activation must not publish an active guest route"
        );
        assert!(matches!(
            *IOAPIC_FORWARDING_ROUTES[guest_gsi].lock(),
            IoApicForwardingRouteState::Prepared(_)
        ));
        assert!(!crate::arch::x86_64::host_irq::test_irq_is_enabled(
            host_irq
        ));
    });
}

#[test]
fn action_enable_failure_remasks_the_activated_device_endpoint() {
    with_clean_forwarding_routes(|| {
        let guest_gsi = 18;
        let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
        TEST_DEVICE_ENDPOINT_UNMASKED.store(false, Ordering::Release);
        register_ioapic_irq_forwarding_route(guest_gsi, host_irq).unwrap();
        register_test_ioapic_forwarding_action(guest_gsi).unwrap();
        super::register_ioapic_irq_forwarding_activation(
            guest_gsi,
            IoApicForwardingActivationOps::new(
                unmask_test_device_endpoint,
                mask_test_device_endpoint,
            ),
        )
        .unwrap();
        fail_next_forwarding_action_enable_for_test();

        activate_ready_ioapic_forwarding_route_for_test(guest_gsi, true)
            .expect_err("host IRQ enable failure must abort route activation");

        assert!(
            !TEST_DEVICE_ENDPOINT_UNMASKED.load(Ordering::Acquire),
            "activation rollback must restore the device-owned INTx mask"
        );
    });
}

#[test]
fn failed_device_revoke_quarantines_route_without_repeating_activation() {
    with_clean_forwarding_routes(|| {
        let guest_gsi = 18;
        let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
        ACTIVATION_COUNT.store(0, Ordering::Release);
        register_ioapic_irq_forwarding_route(guest_gsi, host_irq).unwrap();
        register_test_ioapic_forwarding_action(guest_gsi).unwrap();
        super::register_ioapic_irq_forwarding_activation(
            guest_gsi,
            IoApicForwardingActivationOps::new(count_activation, fail_device_revoke),
        )
        .unwrap();
        fail_next_forwarding_action_enable_for_test();

        activate_ready_ioapic_forwarding_route_for_test(guest_gsi, true)
            .expect_err("failed device revoke must fail route activation");
        activate_ready_ioapic_forwarding_route_for_test(guest_gsi, true)
            .expect_err("a quarantined route must reject an activation retry");

        assert_eq!(
            ACTIVATION_COUNT.load(Ordering::Acquire),
            1,
            "quarantined endpoint ownership must not run device unmask twice"
        );
        assert!(matches!(
            *IOAPIC_FORWARDING_ROUTES[guest_gsi].lock(),
            IoApicForwardingRouteState::Quarantined(_)
        ));
        assert!(!crate::arch::x86_64::host_irq::test_irq_is_enabled(
            host_irq
        ));
    });
}

#[test]
fn failed_activation_batch_rolls_back_only_routes_activated_by_that_batch() {
    with_clean_forwarding_routes(|| {
        let existing_gsi = 16;
        let first_gsi = 17;
        let failing_gsi = 18;
        let existing_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 8);
        let first_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 9);
        let failing_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
        TEST_REVOKED_GSIS.store(0, Ordering::Release);

        register_ioapic_irq_forwarding_route(existing_gsi, existing_irq).unwrap();
        register_test_ioapic_forwarding_action(existing_gsi).unwrap();
        super::register_ioapic_irq_forwarding_activation(
            existing_gsi,
            IoApicForwardingActivationOps::new(count_activation, revoke_existing_test_route),
        )
        .unwrap();
        activate_ready_ioapic_forwarding_route_for_test(existing_gsi, true).unwrap();
        let existing_bit = gsi_bit(existing_gsi);

        register_ioapic_irq_forwarding_route(first_gsi, first_irq).unwrap();
        register_test_ioapic_forwarding_action(first_gsi).unwrap();
        super::register_ioapic_irq_forwarding_activation(
            first_gsi,
            IoApicForwardingActivationOps::new(count_activation, revoke_first_test_route),
        )
        .unwrap();
        register_ioapic_irq_forwarding_route(failing_gsi, failing_irq).unwrap();
        register_test_ioapic_forwarding_action(failing_gsi).unwrap();
        super::register_ioapic_irq_forwarding_activation(
            failing_gsi,
            IoApicForwardingActivationOps::new(fail_activation, revoke_failing_test_route),
        )
        .unwrap();

        activate_ready_ioapic_forwarding_batch_for_test(&[first_gsi, failing_gsi])
            .expect_err("the second route must fail the whole activation batch");

        assert!(matches!(
            *IOAPIC_FORWARDING_ROUTES[existing_gsi].lock(),
            IoApicForwardingRouteState::Active(_)
        ));
        assert!(matches!(
            *IOAPIC_FORWARDING_ROUTES[first_gsi].lock(),
            IoApicForwardingRouteState::Prepared(_)
        ));
        assert!(matches!(
            *IOAPIC_FORWARDING_ROUTES[failing_gsi].lock(),
            IoApicForwardingRouteState::Prepared(_)
        ));
        assert_eq!(
            IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire),
            existing_bit,
            "rollback must preserve pre-existing active routes and remove this batch"
        );
        assert!(!crate::arch::x86_64::host_irq::test_irq_is_enabled(
            first_irq
        ));
        assert!(!crate::arch::x86_64::host_irq::test_irq_is_enabled(
            failing_irq
        ));
        assert_eq!(
            TEST_REVOKED_GSIS.load(Ordering::Acquire),
            gsi_bit(first_gsi) | gsi_bit(failing_gsi),
            "rollback must revoke only device endpoints touched by this batch"
        );
    });
}

#[test]
fn failed_first_run_restores_enable_and_owner_publication() {
    with_clean_forwarding_routes(|| {
        let publication = IoApicForwardingEnablePublication::capture();
        IOAPIC_IRQ_FORWARDING_ENABLED.store(true, Ordering::Release);
        publish_ioapic_forwarding_owner(7, 0);

        restore_ioapic_forwarding_enable_publication(publication).unwrap();

        assert!(!IOAPIC_IRQ_FORWARDING_ENABLED.load(Ordering::Acquire));
        assert_eq!(IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire), usize::MAX);
        assert_eq!(
            IOAPIC_IRQ_FORWARD_VCPU_ID.load(Ordering::Acquire),
            usize::MAX
        );
    });
}

#[test]
fn failed_reenable_restores_the_existing_owner_snapshot() {
    with_clean_forwarding_routes(|| {
        let existing_gsi = 16;
        let existing_bit = gsi_bit(existing_gsi);
        *IOAPIC_FORWARDING_ROUTES[existing_gsi].lock() =
            IoApicForwardingRouteState::Active(test_operations(count_activation));
        IOAPIC_IRQ_ACTIVATED.store(existing_bit, Ordering::Release);
        IOAPIC_IRQ_FORWARDING_ENABLED.store(true, Ordering::Release);
        publish_ioapic_forwarding_owner(3, 2);
        let publication = IoApicForwardingEnablePublication::capture();

        publish_ioapic_forwarding_owner(7, 0);
        restore_ioapic_forwarding_enable_publication(publication).unwrap();

        assert!(IOAPIC_IRQ_FORWARDING_ENABLED.load(Ordering::Acquire));
        assert_eq!(IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire), 3);
        assert_eq!(IOAPIC_IRQ_FORWARD_VCPU_ID.load(Ordering::Acquire), 2);
        assert_eq!(IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire), existing_bit);
        assert!(matches!(
            *IOAPIC_FORWARDING_ROUTES[existing_gsi].lock(),
            IoApicForwardingRouteState::Active(_)
        ));
    });
}

#[test]
fn revoked_active_route_returns_to_prepared_state() {
    with_clean_forwarding_routes(|| {
        let guest_gsi = 18;
        *IOAPIC_FORWARDING_ROUTES[guest_gsi].lock() =
            IoApicForwardingRouteState::Active(test_operations(count_activation));
        let activated = gsi_bit(guest_gsi);
        IOAPIC_IRQ_ACTIVATED.store(activated, Ordering::Release);

        revoke_ioapic_forwarding_routes(activated).unwrap();

        assert!(matches!(
            *IOAPIC_FORWARDING_ROUTES[guest_gsi].lock(),
            IoApicForwardingRouteState::Prepared(_)
        ));
        assert_eq!(IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire), 0);
    });
}

#[test]
fn failed_active_route_revoke_is_quarantined_and_unpublished() {
    with_clean_forwarding_routes(|| {
        let guest_gsi = 18;
        let activated = gsi_bit(guest_gsi);
        *IOAPIC_FORWARDING_ROUTES[guest_gsi].lock() = IoApicForwardingRouteState::Active(
            IoApicForwardingActivationOps::new(count_activation, fail_device_revoke),
        );
        IOAPIC_IRQ_ACTIVATED.store(activated, Ordering::Release);

        revoke_ioapic_forwarding_routes(activated)
            .expect_err("a failed device mask must fail route revocation");

        assert!(matches!(
            *IOAPIC_FORWARDING_ROUTES[guest_gsi].lock(),
            IoApicForwardingRouteState::Quarantined(_)
        ));
        assert_eq!(
            IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire) & activated,
            0,
            "a quarantined route must not remain published as guest-active"
        );
    });
}

#[test]
fn active_route_rejects_configuration_replacement() {
    with_clean_forwarding_routes(|| {
        let guest_gsi = 18;
        let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
        *IOAPIC_FORWARDING_ROUTES[guest_gsi].lock() =
            IoApicForwardingRouteState::Active(test_operations(count_activation));

        let error = register_ioapic_irq_forwarding_route(guest_gsi, host_irq)
            .expect_err("active route configuration must remain immutable");

        assert!(matches!(error, crate::AxVmError::InvalidState { .. }));
        assert_eq!(
            IOAPIC_HOST_IRQS[guest_gsi].load(Ordering::Acquire),
            INVALID_RAW_IRQ,
            "a rejected replacement must not publish partial host IRQ state"
        );
    });
}

#[test]
fn clearing_forwarded_pending_state_preserves_masked_host_line() {
    with_clean_forwarding_routes(|| {
        let guest_gsi = 18;
        mark_forwarded_ioapic_gsi_state(guest_gsi);

        clear_forwarded_ioapic_pending_state(guest_gsi);
        assert_eq!(forwarded_ioapic_gsi_state(guest_gsi), (false, false, true));
    });
}

#[test]
fn forwarded_level_intx_stays_masked_when_guest_eoi_has_deferred_pending() {
    let pending = x86_vlapic::IoApicInterrupt {
        vector: 0x51,
        level_triggered: true,
    };

    assert!(!should_rearm_forwarded_host_gsi_after_eoi(Some(pending)));
}

#[test]
fn forwarded_intx_rearms_host_line_when_guest_eoi_has_no_deferred_level() {
    let pending = x86_vlapic::IoApicInterrupt {
        vector: 0x51,
        level_triggered: false,
    };

    assert!(should_rearm_forwarded_host_gsi_after_eoi(None));
    assert!(should_rearm_forwarded_host_gsi_after_eoi(Some(pending)));
}

#[test]
fn hard_irq_publication_requests_exact_action_disable() {
    with_clean_forwarding_routes(|| {
        let guest_gsi = 18;
        let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
        register_ioapic_irq_forwarding_route_with_trigger(
            guest_gsi,
            host_irq,
            InterruptTriggerMode::LevelTriggered,
        )
        .unwrap();
        register_test_ioapic_forwarding_action(guest_gsi).unwrap();
        publish_ioapic_forwarding_owner(7, 0);

        let published = publish_forwarded_ioapic_irq_fact(
            guest_gsi,
            host_irq,
            0,
            crate::arch::x86_64::host_irq::IrqContext {
                irq: host_irq,
                cpu: irq_framework::CpuId(0),
            },
        );
        let result = forwarded_irq_return_after_wake(WakeResult::Notified);

        assert!(published);
        assert_eq!(
            result,
            crate::arch::x86_64::host_irq::IrqReturn::DisableActionAndWake
        );
        assert_eq!(
            forwarded_ioapic_gsi_state(guest_gsi),
            (true, true, true),
            "the IRQ fact and action-disabled publication must commit before returning"
        );
        assert_eq!(
            forwarded_irq_return_after_wake(WakeResult::Unavailable),
            crate::arch::x86_64::host_irq::IrqReturn::MaskLineAndWake,
            "an unavailable owner must leave the full physical line quenched"
        );
    });
}

fn count_activation() -> crate::AxVmResult {
    ACTIVATION_COUNT.fetch_add(1, Ordering::AcqRel);
    Ok(())
}

fn fail_activation() -> crate::AxVmResult {
    ACTIVATION_COUNT.fetch_add(1, Ordering::AcqRel);
    Err(crate::AxVmError::Interrupt {
        operation: "activate test x86 passthrough route",
        detail: alloc::string::String::from("injected endpoint unmask failure"),
    })
}

fn unmask_test_device_endpoint() -> crate::AxVmResult {
    TEST_DEVICE_ENDPOINT_UNMASKED.store(true, Ordering::Release);
    Ok(())
}

fn mask_test_device_endpoint() -> crate::AxVmResult {
    TEST_DEVICE_ENDPOINT_UNMASKED.store(false, Ordering::Release);
    Ok(())
}

fn fail_device_revoke() -> crate::AxVmResult {
    Err(crate::AxVmError::Interrupt {
        operation: "revoke test x86 passthrough route",
        detail: alloc::string::String::from("injected endpoint mask failure"),
    })
}

fn test_operations(activate: fn() -> crate::AxVmResult) -> IoApicForwardingActivationOps {
    IoApicForwardingActivationOps::new(activate, no_op_revoke)
}

fn no_op_revoke() -> crate::AxVmResult {
    Ok(())
}

fn revoke_existing_test_route() -> crate::AxVmResult {
    TEST_REVOKED_GSIS.fetch_or(gsi_bit(16), Ordering::AcqRel);
    Ok(())
}

fn revoke_first_test_route() -> crate::AxVmResult {
    TEST_REVOKED_GSIS.fetch_or(gsi_bit(17), Ordering::AcqRel);
    Ok(())
}

fn revoke_failing_test_route() -> crate::AxVmResult {
    TEST_REVOKED_GSIS.fetch_or(gsi_bit(18), Ordering::AcqRel);
    Ok(())
}

fn mark_forwarded_ioapic_gsi_state(guest_gsi: usize) {
    if should_register_ioapic_gsi_hook(guest_gsi) {
        let bit = gsi_bit(guest_gsi);
        IOAPIC_IRQ_PENDING.fetch_or(bit, Ordering::AcqRel);
        IOAPIC_IRQ_PENDING_LEVEL.fetch_or(bit, Ordering::AcqRel);
        IOAPIC_IRQ_ACTION_DISABLED.fetch_or(bit, Ordering::AcqRel);
    }
}

fn forwarded_ioapic_gsi_state(guest_gsi: usize) -> (bool, bool, bool) {
    if !should_register_ioapic_gsi_hook(guest_gsi) {
        return (false, false, false);
    }

    let bit = gsi_bit(guest_gsi);
    (
        IOAPIC_IRQ_PENDING.load(Ordering::Acquire) & bit != 0,
        IOAPIC_IRQ_PENDING_LEVEL.load(Ordering::Acquire) & bit != 0,
        IOAPIC_IRQ_ACTION_DISABLED.load(Ordering::Acquire) & bit != 0,
    )
}

fn with_clean_forwarding_routes(test: impl FnOnce()) {
    let _guard = ROUTE_TEST_LOCK.lock();
    reset_forwarding_routes();
    test();
}

fn reset_forwarding_routes() {
    for slot in &IOAPIC_IRQ_HANDLES {
        let handle = slot.lock().take();
        if let Some(handle) = handle {
            crate::arch::x86_64::host_irq::free_irq(handle)
                .expect("test forwarding action must retain its generation-owned handle");
        }
    }
    crate::arch::x86_64::host_irq::reset_test_irq_enable_state();
    for host_irq in &IOAPIC_HOST_IRQS {
        host_irq.store(INVALID_RAW_IRQ, Ordering::Release);
    }
    IOAPIC_HOST_IRQ_EXPLICIT.store(0, Ordering::Release);
    IOAPIC_HOST_IRQ_LEVEL_TRIGGERED.store(0, Ordering::Release);
    IOAPIC_IRQ_PENDING.store(0, Ordering::Release);
    IOAPIC_IRQ_PENDING_LEVEL.store(0, Ordering::Release);
    IOAPIC_IRQ_ACTION_DISABLED.store(0, Ordering::Release);
    IOAPIC_IRQ_OWNER_BOUND.store(0, Ordering::Release);
    IOAPIC_IRQ_OWNER_CPU.store(usize::MAX, Ordering::Release);
    IOAPIC_IRQ_OWNER_THREAD_ID.store(u64::MAX, Ordering::Release);
    IOAPIC_IRQ_ACTIVATED.store(0, Ordering::Release);
    reset_forwarding_action_enable_failure_for_test();
    IOAPIC_IRQ_FORWARDING_ENABLED.store(false, Ordering::Release);
    IOAPIC_IRQ_HOOK_REGISTERED.store(false, Ordering::Release);
    IOAPIC_IRQ_FORWARD_VM_ID.store(usize::MAX, Ordering::Release);
    IOAPIC_IRQ_FORWARD_VCPU_ID.store(usize::MAX, Ordering::Release);
    IOAPIC_ROUTE_TRANSACTION_ACTIVE.store(false, Ordering::Release);
    for route in &IOAPIC_FORWARDING_ROUTES {
        *route.lock() = IoApicForwardingRouteState::Vacant;
    }
}
