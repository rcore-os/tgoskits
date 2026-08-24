# T3.2 vGIC LR Exhaustion and Maintenance Verification

## Conclusion

The current GICv3 backend already retries software-pending virtual interrupts
when list registers (LRs) become available. No Task1-specific EOI retry path is
needed. Adding a second retry mechanism would duplicate ownership and risk
replaying edge-triggered interrupts.

## Delivery Path

```text
software interrupt pending outside LRs
        |
        v
RedistributorState::configure_delivery_traps
        | UIE + NPIE (pending spill)
        | UIE + LRENPIE + TDIR (active spill)
        v
guest EOI / LR state change -> host VGIC maintenance PPI
        |
        v
ArmVcpu::run saves ICH state on every exit
        |
        v
merge completed LRs and queued state
        |
        v
next binding.load refills empty LRs before guest re-entry
```

The host maintenance PPI is discovered from the host FDT and enabled per CPU
by `virtualization/axvm/src/arch/aarch64/gic/maintenance.rs`. The acknowledged
maintenance interrupt is consumed by the VGIC route instead of being exposed
as a guest physical interrupt.

## Code Evidence

- `virtualization/arm_vgic/src/cpu_interface.rs`
  `configure_delivery_traps()` owns UIE, LRENPIE, NPIE, TDIR, and EOI-count
  state. Pending deliveries outside LRs set UIE+NPIE; active spill sets
  UIE+LRENPIE and requests deactivation trapping.
- `virtualization/arm_vgic/src/redistributor/mod.rs`
  derives those trap bits from queued deliveries and saved LR state.
- `virtualization/arm_vgic/src/controller/binding.rs`
  `load()` refills empty LRs before guest entry. `save()` harvests completed
  hardware LRs after every guest exit. `synchronize()` is the explicit
  save/merge/refill/reload operation for callers that need the whole cycle in
  place.
- `virtualization/axvm/src/arch/aarch64/mod.rs`
  wraps every guest run with VGIC `load()` and `save()`. A maintenance exit is
  therefore folded before the vCPU run loop re-enters the guest.
- `virtualization/axvm/src/arch/aarch64/gic.rs`
  recognizes the maintenance PPI and deactivates it without routing it as a
  guest-assigned IRQ.

## Regression Evidence

`virtualization/arm_vgic/tests/gicv3_delivery.rs` contains
`lr_exhaustion_queues_and_refills_without_repeating_completed_edges`:

1. A one-LR backend receives two edge SPIs.
2. The first occupies the LR and the second remains software-pending.
3. UIE and NPIE are asserted while work is outside the LR.
4. Completing the first LR and synchronizing loads the second exactly once.
5. Completing the second leaves no LR and no pending interrupt.

The same suite also verifies active-LR spill, LRENPIE, TDIR, priorities, and
physical-backed interrupt retirement. These tests exercise the shared VGIC
state machine rather than a Task1-only fast path.

## Boundary

This verifies architectural retry correctness and immediate refill at the
next guest entry. QEMU TCG does not establish a hardware upper bound for the
maintenance interrupt latency itself; physical-board timing remains separate
work.
