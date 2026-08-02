# Arm VGIC SPI Delivery

## Problem and users

AxVM needs to emulate routed Arm shared peripheral interrupts without treating
ICH list registers as durable interrupt state. Device producers and routing are
outside this change; the immediate users are the future AxVM routing layer, the
GIC distributor wrapper, and the AArch64 vCPU run loop.

Success means that edge and level state survives LR eviction and vCPU
save/restore, completion is attributed to the correct delivery instance, a full
LR bank is refilled after maintenance, and no IRQ-time path allocates or creates
an interrupt implicitly.

## Scope and non-goals

This design provides:

- a sealed, sparse controller for registered SPI INTIDs 32 through 1019;
- target-local LR installation, observation, reconciliation, and refill;
- bounded ICH access for AxVM;
- optional GICv3 maintenance-PPI discovery and observation; and
- EOImode0 completion plus trapped `ICC_DIR_EL1` deactivation.

It does not create production fixed routes, a Router, a sender, or device
producers. It also does not implement `IROUTER`, clear-enable, pending/active
distributor registers, or active interrupts outside LRs. Production composition
belongs to the following routing change.

## Prior art and alternatives

The architectural basis is Arm GICv3 virtualization, especially the LR state,
MISR/EISR, UIE, and TDIR behavior in
[Arm IHI 0069H.b](https://documentation-service.arm.com/static/67eaa4d098aa3c3b6eea7351).
Linux and other VMM implementations likewise distinguish software interrupt
state from the finite hardware LR cache.

Keeping all state only in saved LRs was rejected because it cannot represent
pending work outside a full LR bank and makes stale completion indistinguishable
from current delivery. Reusing direct injection alone was rejected because it
has no routed source state or completion ownership. Allocating records on an
IRQ path was rejected because an unknown INTID must be an error, not implicit
configuration.

## Durable state model

Each registered SPI keeps independent fields for its route target, trigger
mode, enable state, edge pending latch, level assertion, active state, resident
owner/epoch, and requested deactivation. Registration is allowed only before
the controller is sealed. After sealing, all service paths operate on existing
records under a short `SpinNoIrq` critical section.

An LR is an execution cache. `deliver_one` selects deliverable state, invokes a
target-local installer while the state is locked, and commits pending
consumption plus a new epoch only after the installer succeeds. Failed LR writes
therefore leave both source state and epoch unchanged.

The vCPU delivery port owns a slot map from module-owned LRs to
`(INTID, epoch, EOI-maintenance)`. Valid unmapped LRs remain compatible with
existing SGI/PPI and direct-injection paths. An EISR bit for an unmapped LR, or
for a mapped edge LR whose EOI-maintenance bit was not installed, is malformed
because neither slot can legitimately produce that completion.

Before guest entry, AxVM folds observed LR state, reconciles line or
deactivation changes, refills empty slots, and updates owned HCR controls. It
folds again after exit and once before unbind. UIE is enabled only when
deliverable work remains after refill; emulated delivery rejects hardware with
fewer than two common LRs.

`ICC_DIR_EL1` trapping is enabled only through the delivery session and only
when TDIR is supported. A local module-owned SPI creates an epoch-bound
deactivation command and reconciles immediately. A remote owner returns
a typed target hint without touching a remote LR; the current runtime maps that
hint to `Unsupported` until the later routing change supplies a sender. Private
or unregistered interrupts use the compatibility LR state transition.

## ICH capability boundary

`ArmVcpu::with_bound_ich` creates a non-escaping `IchSession` only for the
currently bound host CPU. The session exposes typed LR operations,
MISR/EISR snapshots, and UIE/TDIR controls. It cannot expose the register
backend or write arbitrary HCR bits. EN is managed by the session; NPIE,
LRENPIE, EOICOUNT, and other unowned fields remain zero. GICv2 returns
`Unsupported` without accessing ICH registers.

## Maintenance IRQ ownership

The GICv3 probe copies the unique maintenance interrupt specifier and, after
registering the exact controller, translates and configures it in that
controller's dynamic IRQ domain. Only a same-domain PPI in INTID range 16
through 31 is published as a typed `IrqId`. The platform boundary exposes the
write-once status as uninitialized, available, unavailable, or error; an absent
specifier is therefore distinct from a controller returning
`IrqError::Unsupported`, and lock contention remains `Error(Busy)`. Missing or
malformed optional capability state does not undo a working host GIC.

AxVM registers one host-lifetime per-CPU handler after virtualization is enabled
on all usable CPUs and exposes a read-only status distinguishing uninitialized,
registered, unavailable, and the original registration error. Querying the
status cannot retry registration. Bind publishes `(VMId, VCpuId, generation)`
under local IRQ exclusion. The handler records only a matching observation; it
never accesses LRs or the controller. Unbind consumes the observation, services
and saves ICH, then withdraws ownership. The normal platform IRQ transaction
remains the only acknowledge/EOI owner. `ArmHostOps` reports transaction
ownership together with the fetched vector: AArch64 fetch completes
acknowledge, dispatch, and EOI before returning `FetchHandled`, so deferred work
checks timers without a second dispatch.

## Validation

Deterministic unit tests cover INTID bounds, controller sealing, edge merging,
level lowering and requeue, enable-after-pending, installer/apply rollback,
epoch mismatch, deactivation, LR capacity and register validation, maintenance
reason validation, and HCR ownership. Target builds check the AArch64 soft-float
configuration; host tests preserve non-AArch64 AxVM behavior. The existing
AArch64 passthrough smoke remains a compatibility regression, while a routed
device end-to-end test is intentionally deferred until production routing is
available.
