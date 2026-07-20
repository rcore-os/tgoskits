# Cross-Kernel Driver Architecture Reference

Read this when creating, optimizing, or reviewing drivers under `drivers/`.

## Source Principles

The core problem is OS API coupling, not Rust portability. Hardware logic is usually stable; lock types, task models, DMA allocation/sync, MMIO remap, and IRQ registration vary by kernel.

Keep this mapping:

- Driver Core: registers, register access order, state machine, descriptor format, queue logic, request completion, event extraction.
- Capability Boundary: OS Trait / Driver Trait seam for MMIO, DMA, IRQ event, queue contract, wake boundary.
- OS Glue: probe, remap/iomap, IRQ registration, FDT/ACPI/PCI discovery, thread or worker spawn, OS wakeup APIs.
- Runtime: CPU-pinned maintenance domains, request tables, blocking facades, watchdogs, and recovery orchestration. Generic shared workers do not advance stateful device queues.

Do not pursue one big `KernelHal` in production code. Split by lifetime and semantics: MMIO, DMA, IRQ, task progression, and queues usually have different owners and hot paths.

## Project Layout

Reusable hardware/IP crates live under `drivers/` by device type:

```text
drivers/<device-type>/<crate-name>
drivers/<device-type>/<vendor-or-family>/<crate-name>
```

Use descriptive device-type directories for reusable crates. For example, a new reusable block device may live under `drivers/block/<vendor>/<crate>`. Existing platform glue may use shorter historical module names such as `platforms/axplat-dyn/src/drivers/blk`; keep those names when editing that layer.

Existing examples:

- `drivers/npu/rockchip-npu`
- `drivers/soc/rockchip/rockchip-pm`
- `drivers/soc/rockchip/rockchip-soc`

When adding a new reusable crate:

1. Add it to root workspace `members`.
2. Add it to root `[workspace.dependencies]` if other workspace crates consume it.
3. Keep crate `src/` portable and `#![no_std]` friendly when practical.
4. Keep OS-specific crates out of normal `[dependencies]`.
5. Prefer `<dep>.workspace = true` in member `Cargo.toml` files when the dependency is already present in root `[workspace.dependencies]`; avoid direct version duplication for new code.

Runtime/platform integration belongs elsewhere:

- `platforms/axplat-dyn/src/drivers/<type>/...` for dynamic platform/FDT/probe glue.
- `platforms/axplat-dyn/src/drivers/soc/<vendor>/...` for SoC platform glue.
- `components/axdriver_crates/axdriver_<type>` for common ArceOS-facing driver traits/adapters.

For block devices on the dynamic platform path, the integration path is:

- probe/FDT/PCI/MMIO setup in `drivers/ax-driver/src/block/<driver>.rs`
- expose the portable driver as `rdif_block::Interface`
- expose queues as `rdif_block::IQueue` with owned submission and IRQ-event service
- expose discovery-to-ready hardware work as a bounded `ControllerInitEndpoint`
- register boxed `rdif_block::Interface` devices through the ArceOS driver glue and `rdrive`
- keep tags, waiters, CPU-pinned maintenance threads, watchdogs, recovery, and IRQ registration in `ax-runtime`

Keep `rdrive` coupling in this adapter layer or behind an explicit adapter feature. Portable driver core should not need to know how `rdrive` probes or registers devices.

## Dependency Boundaries

Use `dma-api` and `mmio-api` as portable capability APIs.

Current versions observed on 2026-04-28:

```toml
mmio-api = "0.2.1"
dma-api = "0.7.2"
```

Before changing versions, run:

```bash
cargo search mmio-api --limit 5
cargo search dma-api --limit 5
cargo info mmio-api
cargo info dma-api
```

`dma-api` is already present in this workspace root. `mmio-api` may need to be added to root `[workspace.dependencies]` if new code adopts it.

## MMIO Pattern

Portable core should receive an already mapped register region or typed wrapper. Avoid direct calls to `axklib::mem::iomap`, `bare_test::mem::iomap`, Linux `ioremap`, or any target OS mapping API from reusable driver code.

Use `mmio-api` in glue:

- Implement `mmio_api::MmioOp` for the target OS/platform.
- Call `mmio_api::ioremap` or equivalent glue-side mapping during probe/setup.
- Pass `mmio_api::Mmio`, `mmio_api::MmioRaw`, `NonNull<u8>`, or a typed register wrapper into Driver Core.
- Keep `mmio_api::MmioRaw::new`, raw pointer casts, and map lifetime assumptions inside the boundary.

Existing `axplat-dyn` glue has local helpers such as `crate::drivers::iomap` that return `NonNull<u8>`. For new code, prefer wrapping the OS mapping operation in a `mmio_api::MmioOp` implementation. When adapting an existing driver that still takes `NonNull<u8>`, it is acceptable to map with `mmio-api` in glue and pass `MmioRaw::as_nonnull_ptr()` while keeping the owning `Mmio`/mapping lifetime in the adapter.

Driver core may use volatile register access through `mmio-api`, `tock-registers`, or a small typed wrapper matching the existing crate style.

## DMA Pattern

OS Glue implements:

```rust
impl dma_api::DmaOp for DmaImpl { /* platform allocator and cache ops */ }
```

Device setup creates:

```rust
let dma = dma_api::DeviceDma::new(dma_mask, &DMA_IMPL);
```

Driver Core should use `DeviceDma` and `dma-api` abstractions:

- `DArray` / `DBox` for coherent descriptor rings, command buffers, and fixed DMA-owned data.
- `map_single_array` / `SArrayPtr` for mapping existing buffers.
- `DmaDirection::{ToDevice, FromDevice, Bidirectional}` to make cache sync semantics explicit.
- `DmaAddr` for device-visible bus addresses.

Check every DMA path for:

- mask/address width
- alignment
- page/layout size
- cache flush/invalidate direction
- map/unmap or alloc/dealloc pairing
- ownership transfer and zero-copy lifetime
- distinction between CPU virtual address and bus/DMA address

## IRQ/Event Pattern

Portable IRQ handling should answer "what happened?" OS Glue answers "how should execution continue?"

Use an IRQ endpoint that extracts a stable event and can fail closed at its exact device source. When the endpoint has mutable runtime state, prefer `&mut self` and let the OS IRQ registration own the endpoint:

```rust
pub trait IrqEndpoint {
    type Event;
    type Fault;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault>;
    fn contain(
        &mut self,
        cause: ContainmentCause,
    ) -> Result<MaskedSource, Self::Fault>;
}
```

For stateless raw event extractors, a free function can still be appropriate. Do not make a stateful IRQ endpoint clonable merely so registration code can keep a pointer to it.

`Event` should identify:

- event kind
- affected queue or engine
- completion state
- error or recovery state

The IRQ fast path should:

- identify the interrupt source
- read and clear required status registers
- return a stable event object
- avoid blocking, long work, and broad locks

OS Glue publishes events into a fixed mailbox and wakes the same-CPU maintenance owner through a pre-saved local IRQ wake. Completion timers report timeout and wake that owner; they never inspect device completion state.

### IRQ Callback Ownership Pattern

If an IRQ handler endpoint is only meaningful inside the registered interrupt callback, encode that in ownership:

```rust
let owner = pin_current_cpu()?;
let mut irq = parts.irq;
let registration = registrar.register(irq_number, move |ctx| {
    let capture = irq.capture();
    publish_and_wake_local(ctx, capture)
})?;
```

Use the target kernel's equivalent of a boxed `FnMut` callback, registration token, or owned closure. The important property is not the allocation mechanism; it is that the IRQ framework owns the handler for the registration lifetime and calls it non-reentrantly. This gives the handler a single mutation site without `Arc<Mutex<_>>`, raw pointer lifetime tricks, or public APIs that unrelated task code can call.

When applying this pattern:

- Register from the maintenance thread after its CPU lease is live, with IRQ affinity fixed to that same CPU.
- Register with a non-reentrant IRQ execution contract if the callback mutates captured state.
- Count the move-only IRQ action itself as a live maintenance capability, independently of any wake or endpoint captured by its closure. Active-to-detached-to-reattached transitions transfer that one capability; only a successful explicit action close releases it. An implicit drop must quarantine the domain without releasing the CPU lease.
- Drop the captured handler when the IRQ action is freed, after in-flight callbacks are synchronized.
- Keep hard-IRQ work small: read/ack status, update only endpoint-local capture/mask generation, and publish a minimal stable event into the fixed mailbox. Queue state belongs to the maintenance thread.
- Do not capture OS objects that require allocation, sleeping locks, broad poll-set locks, or file/device-manager callbacks in hard IRQ. Publish into the fixed maintenance mailbox and use only its saved local-owner wake.
- Keep task-side service/config methods on a separate control endpoint owned only by the maintenance thread. If they also touch registers, protect them with same-owner CPU local IRQ exclusion or a documented borrow gate that prevents IRQ reentry. Remote callers submit messages; they never call the endpoint directly.

This ownership model is useful beyond serial ports: block completion queues, network RX/TX interrupt endpoints, input devices, accelerators, and mailbox controllers all benefit when "the IRQ handler" is not a shared runtime object.

Exclusive device handoff needs a stronger operation than disabling the host
action. After device-side masking and action drain, remove the action from its
descriptor and retain its move-only callback in a linear detached-action token.
This lets a guest install an action with its own sharing and affinity contract;
a disabled host action left in the descriptor would still participate in those
compatibility checks and would represent a second IRQ owner. Guest return must
first revoke and free every guest action, then reattach the retained host token
as a disabled action before controller reinitialization can wait for IRQs. A
failed detach or reattach keeps the unique action/token owner in quarantine;
never solve the conflict by weakening descriptor compatibility.

### Control / IRQ / Queue Endpoint Pattern

For IRQ-driven runtime drivers, split runtime ownership into three endpoint families:

```text
Control endpoint  -> startup, shutdown, config, bounded service and rearm
IRQ endpoint      -> hard-IRQ event extraction, acknowledgement, and containment
Queue endpoints   -> submit and consume only synchronized IRQ/event state
```

The split keeps each synchronization question local:

- Control endpoint: owned by exactly one CPU-pinned maintenance thread; may call slow OS services through OS Glue outside IRQ context.
- IRQ endpoint: owned by IRQ registration; reads and clears shared/destructive IRQ status, maintains only its capture/mask generation, and emits stable Copy events. It does not complete requests or mutate queue state.
- Queue endpoint: owned only by the CPU-pinned maintenance thread; consumes mailbox events and advances permits, completions, errors, submission, and recovery without borrowing the IRQ endpoint. Remote runtime users submit owned requests to the domain ingress instead of calling it.

Return these parts explicitly from constructors:

```rust
pub struct DeviceParts<Q> {
    pub control: ControlPort,
    pub irq: IrqHandler,
    pub queues: Q,
}
```

Move all parts into the selected maintenance thread before registration. The callback takes ownership of `irq`; the thread retains `control` and queues. If task and IRQ paths overlap one register block, OS glue may use a same-CPU local-IRQ exclusion cell, but it must not expose that cell as a remotely callable driver object.

Review smell: if task code calls `irq.capture()` directly, if IRQ code locks a remotely accessible queue mutex, or if queues call a raw `poll_status()` that can clear another queue's event, the endpoint split is not yet enforcing the intended model.

### IRQ/Task Exclusion Pattern

Some drivers need IRQ completion code to consult state that is registered or
removed by task context, such as xHCI transfer rings or queue completion slots.
The safe shape is an explicit two-sided protocol:

- Task context masks or disables the exact device interrupt source that can run
  the IRQ path, then takes the mutation lock and updates the registry.
- IRQ context does not take that mutation lock. It only uses a narrowly scoped
  fast-path accessor over entries whose lifetime was established before the
  interrupt was enabled.
- The fast-path accessor is unsafe or otherwise documented with the required
  exclusion/lifetime contract.
- The shared state contains stable descriptors, queue slots, atomics, or ready
  bits; it must not require allocation, blocking, broad OS locks, or callbacks
  into file/device managers while in IRQ context.
- Re-enable the interrupt source only after task-side mutation has fully
  published the new state.

This protocol prevents the classic deadlock where task context holds a spinlock,
gets interrupted by the same device, and the IRQ handler tries to acquire the
same lock. It is narrower than "IRQ-safe" in general: it does not make arbitrary
wakers, heap allocation, sleeping locks, or unrelated subsystem locks safe in a
hard IRQ. If the driver cannot prove this protocol, fail activation and redesign
the endpoint split. Lock contention is not a valid IRQ event and must not create
a deferred acknowledgement path.

### IRQ/Queue Isolation Pattern

For devices with split runtime endpoints, treat the IRQ handle as a state synchronizer, not as a queue owner:

- Give the IRQ handle its own endpoint object, separate from TX/RX queues, completion queues, block queues, network rings, or accelerator engines.
- Let the IRQ handle be the only runtime path that reads and clears shared or destructive interrupt/status registers. Queue-side code should not rediscover readiness by peeking the same global register, because that can clear or consume another queue's event.
- Encode the affected queue or engine and errors in the stable IRQ event. Only the maintenance thread fans those facts into queue-local completion state; the callback must not race the queue owner by doing that fan-out itself.
- Keep TX/RX, submit/completion, or per-queue completion states independent even when hardware reports them in one combined status register. Split combined status immediately in the IRQ endpoint before queue code observes it.
- An IRQ must never arrive while a remote or sleepable context owns the raw register block. Same-CPU owner access disables the precise local/device source before mutation. Violation is a containment fault, not a retry request.
- Keep raw driver event snapshots close to hardware semantics. Put OS wakeups, task scheduling, and per-queue completion ownership in the adapter/runtime layer above the raw register code.

## Queue/Runtime Pattern

Model queues as independent running units. This matches network TX/RX queues, NVMe admin/IO queues, block request queues, and many accelerator command queues.

Common block actions:

- `submit_owned`
- `service_events` from an acknowledged IRQ snapshot
- `reclaim_after_quiesce` with a typed generation/controller proof
- `shutdown` only after ownership has already returned

Every hardware request first transfers into the owning maintenance domain. A tagged queue may keep several requests in flight and a serialized queue may keep one, but only that domain's pinned thread calls the driver. Independent hctx/vector domains may use separate threads; queues that share one IRQ/control state remain in one domain.

Avoid a single global `Driver::poll` if the hardware naturally exposes multiple queues or engines. Avoid a "big object + big lock + callbacks" shape unless the device is truly that simple.

In an IRQ-driven split design, queue operations consume synchronized queue-local state:

- `service_events` may consume only the event snapshot routed to that queue. It must not read or clear global hardware IRQ status or retry a destructive acknowledgement.
- Queue `submit`/`try_write` should consume that queue's own permits or descriptor budget and then program only the register path needed to advance that queue.
- Queue `reclaim`/`try_read` should consume that queue's own completion or error state. Do not let one queue consume another queue's event because a shared register reported combined status.
- If a hardware status register reports multiple queues or directions in one destructive read, split that status immediately in the IRQ/event layer and store independent queue-local state before any queue code runs.
- For FIFO-style devices where one readiness interrupt may cover a bounded burst, model the budget explicitly if more than one operation can be performed. Avoid hidden loops that re-read global status from a queue path.
- Initialization state machines may recheck reset/clock/power state at an absolute deadline. Normal I/O has no completion-polling path: a lost IRQ ends in watchdog failure and controller recovery.

For a block queue adapter, align portable queue state with `rdif_block::IQueue`:

- `QueueInfo` exposes block size, alignment, DMA mask, watchdog, completion kind, and `QueueExecution::{Inline, Serialized, Tagged}`.
- `submit_owned(id, request)` either returns the complete request inline or transfers ownership exactly once.
- `service_events(events, sink)` consumes bounded IRQ evidence and emits terminal `CompletedRequest` values.
- timeout/cancel does not return DMA ownership; recovery must first produce `DmaQuiesced` for the matching controller epoch.
- Before an interrupt queue is published, bind its `QueueHandle` once to the
  retained controller identity and publication epoch. The generic handle must
  reject an unbound queue, a proof from another controller, a proof that is not
  strictly newer than publication, and a replayed or older proof before driver
  code can reclaim ownership; the driver should validate the same cookie/epoch
  again as defense in depth.
- Keep descriptor ownership and DMA map/unmap pairing explicit for every generation-bearing request ID.

## Concurrency Rules

- Prefer `&mut self` for externally visible operations that require exclusive access.
- Do not make OS locks part of the portable Driver Trait.
- Do not take a blocking mutex from an IRQ handler when task context can hold the same mutex. Use the same-CPU owner and local-IRQ exclusion contract or split the endpoint state.
- Use internal locks only for short non-IRQ critical sections such as pending flags or small status updates. In IRQ context, prefer atomics, fixed mailboxes, and local owner wakeups.
- Do not hide IRQ endpoint sharing behind `Arc<Mutex<_>>` or `Arc<SpinLock<_>>` when the endpoint can instead be moved into the IRQ callback. Shared locks make it too easy for task context to call or hold the same state the hard IRQ needs.
- If task and IRQ contexts share a lock-protected registry, require the IRQ/task
  exclusion protocol above: mask the same interrupt source before task-side
  mutation, keep IRQ lock-free for that registry, and document why the fast path
  cannot race lifetime or structure changes.
- When one raw register block is shared by several queues or endpoints, centralize mutable register access in one core object. Wrap it in `UnsafeCell` or another narrow unsafe primitive only in the adapter/runtime layer, document the exclusion rule, and avoid exposing unsynchronized raw access to queues.
- Separate synchronization ownership from hardware logic. The raw driver should expose register-level primitives and stable event snapshots; the runtime/adapter should decide how IRQ, queues, pending state, and wakeups are synchronized.
- Treat a composite device and its transport as one physical ownership bundle when they share control state or IRQ affinity. For example, AIC Wi-Fi over SDIO exposes firmware/device FSM, RX/TX queues, and an IRQ event endpoint, while OS glue supplies one generic network maintenance domain. The portable crate must not spawn device-specific threads or retain OS wakers/workqueue handles.
- Keep `unsafe` in callback bridges, MMIO construction, and DMA glue boundaries where possible.
- Do slow work in the bounded maintenance-thread loop, not in IRQ context. Do not introduce a normal-I/O polling context as a fallback.

## Review Checklist

- Does `src/` stay OS independent?
- Are OS crates limited to `dev-dependencies`, platform glue, or explicit adapter crates?
- Is MMIO mapping handled by `mmio-api` or a clear OS Glue boundary?
- Is DMA handled through `dma-api` with mask, alignment, direction, lifetime, and address-type clarity?
- Does IRQ code return events rather than directly performing OS notification?
- Is a stateful IRQ endpoint owned by the registered callback instead of shared through a public `Arc` or lock?
- Are control, IRQ handler, and queue endpoints separated with clear owners?
- Do queues consume queue-local synchronized state rather than re-reading shared/destructive IRQ registers?
- Does each accepted request reach exactly one terminal completion, with DMA ownership returned only through the declared inline/IRQ and quiescence contracts?
- Does discovery defer the first hardware command until the maintenance owner and IRQ actions are bound, and does every pending initialization state name `run_again`, IRQ sources, or an absolute deadline?
- Did validation include `cargo fmt` and targeted `cargo xtask clippy --package <crate>`?
