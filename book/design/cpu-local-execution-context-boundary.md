# CPU-local execution-context boundary

## Problem

PR #1775 needs a stable low-level current/preemption mechanism, but its task and
runtime refactor must not become an implicit dependency of `cpu-local`. The old
surface also encoded upper-layer concepts in names and data: current task
identity, scheduler publication, and task-owned preemption counters. Copying
that directory would reverse the intended dependency and make the mechanism
impossible to reuse outside one scheduler implementation.

This design establishes the independently mergeable boundary used by current
ArceOS and later by #1775. It changes no syscall, scheduling policy, task API,
or `someboot` startup behavior.

## Users and success criteria

The direct users are architecture switch/trap code, `ax-runtime`, and typed
per-CPU layout code. Task systems consume the runtime capability rather than
the architecture mechanism.

The boundary succeeds when:

- `cpu-local` has no `ax-task` or `ax-runtime` dependency and exposes no
  scheduler-named operation, runtime cookie, task owner, run queue, IPI, or
  baton;
- every final image has one authoritative current-context source;
- an execution context can be embedded at offset zero in a runtime-owned
  wrapper without a second identity publication;
- switch preparation rolls back when abandoned and stale previous bindings
  cannot be consumed twice;
- preemption entry and pending exit are linear; context-owned tokens retain
  their exact state owner, while a CPU-owned switch token can transfer only at
  the context-switch resume boundary;
- existing ArceOS consumers build and run without changing scheduling policy.

## Non-goals

This change does not import #1775's TaskSystem, RT/Deadline policy, IPI delivery,
thread publication, or scheduler implementation. It does not add physical-board
behavior, CPU hotplug, a versioned runtime ABI, or a compatibility cookie.

## Ownership and dependency direction

`cpu-local` owns fixed CPU-area metadata, current-context register selection,
context CPU binding, switch transactions, and the preemption word.
`ax-percpu` owns typed layout, template initialization, and layout freeze.
`ax-runtime` owns IRQ exclusion, bootstrap release, the scheduler baton, and the
safe-point adapter. The task layer owns `need_resched`, `force_resched`, run
queues, policy, and remote requests.

The legacy ArceOS task implementation uses a narrow `RuntimePreemption`
capability so the runtime, not the task crate, consumes CPU-local tokens. PR
#1775 can replace that compatibility capability with `TaskRuntime` without
changing the CPU-local API.

## Current-context sources

| Architecture/image | Authoritative current source |
| --- | --- |
| x86_64, with or without TLS | CPU runtime anchor addressed through GS |
| AArch64, with or without TLS | SP_EL0 |
| RISC-V/LoongArch64 without TLS | architecture `tp` register |
| RISC-V/LoongArch64 with TLS | CPU runtime anchor |

The CPU anchor remains storage for x86_64 and TLS-conflicting architectures; it
is not a mirror in modes that use an architecture register. AArch64 user entry
temporarily borrows SP_EL0 for user SP and therefore spills the header in the
pinned kernel stack until exception return restores it.

`ExecutionContextHeader` begins with its CPU-area binding. Runtime wrappers use
`#[repr(C)]` and place the header first. The current header address is the sole
architecture identity; a wrapper may store its own task owner beside it.

## Switch transaction

`prepare_context_switch` validates the outgoing source, captures its binding
epoch, and binds the incoming context. `PreparedContextSwitch` owns rollback of
the incoming binding until `commit`. `PreviousContextBinding` owns exactly one
withdrawal of the outgoing epoch after the raw switch. Architecture-register
modes write current only in the naked tail; anchor modes publish the atomic slot
at commit. No fallible or ownership-sensitive Rust work follows commit.

## Preemption transaction

The preemption word uses an inverted pending bit and a nesting depth. x86_64
stores it in the fixed CPU anchor, while load/store architectures store it in
the execution-context header. `PreemptionToken` captures the selected word at
entry. Ordinary exit finishes that exact owner; only the explicit x86_64
context-switch resume handoff may replace a CPU-owned switch token.

`finish_preemption` consumes a nested or non-pending final depth. A final
pending exit returns `PendingPreemption` while depth one remains published.
`ax-runtime` masks local IRQs, mirrors task work into the pending bit, claims a
per-CPU scheduler baton, releases the pending depth, clears the mirror, and only
then invokes the task safe point. The task layer never manipulates the word and
`cpu-local` never decides whether to schedule.

Bootstrap contexts start at depth one. The runtime releases that exact depth
once, after current context and local run-queue state are published.

On x86_64, a runtime guard's CPU-owned exclusion covers the raw context switch.
If the suspended context resumes on the same CPU, its token retains the same
owner. If it resumes on another CPU, the old CPU's incoming context has consumed
the old switch depth and the destination CPU's outgoing context has left an
equivalent depth. The incoming runtime tail therefore consumes the old linear
proof and uses `handoff_preemption_after_context_switch` to adopt the destination
CPU's depth before finishing the guard. A context running for the first time has
no suspended caller, so its first-entry runtime tail invokes
`release_initial_context_preemption`. Load/store architectures migrate the
context-owned word itself, reject any owner change, and start a new context at
depth zero.

## Alternatives rejected

- Keeping a task pointer in both an architecture register and CPU anchor was
  rejected because the two values can diverge across traps, user transitions,
  or vCPU exits.
- Keeping a runtime cookie in `ExecutionContextHeader` was rejected because the
  wrapper address already provides identity and the cookie leaks ownership
  upward.
- Keeping the preemption counter in the task object was rejected because x86_64
  requires CPU ownership while other architectures require context ownership.
- Copying or cherry-picking #1775 was rejected because it also carries task
  system and scheduler changes that are not independently required here.

## Evidence

Host tests cover dependency vocabulary, current-source selection, offset-zero
embedding, switch rollback and stale epochs, nested/final/pending preemption,
bootstrap depth, malformed raw tokens, and owner retention. Cross-target clippy
checks all four architecture backends. ArceOS QEMU covers task switching and
preemption; Starry covers non-TLS current modes; Axvisor covers TLS modes.
