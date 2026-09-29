Continue your read-only audit from the previous session. Do not edit,
build, test, run boards or launch agents. Your first answer has useful
anchors but the main reviewer found correctness gaps. Repair them with
exact source references and either a concrete implementable design or
an explicit conclusion that reliable pairing is too invasive.

Corrections required:
1. `ThreadWakeBatch::wake_all` (`components/ax-task/src/thread/handle/wake_batch.rs`)
   is generic and has no futex address. `ResolvedFutex::wake` knows `self.key`
   and `WakeBatch::push` receives the handle, but the batch is opaque.
   Show exactly where the gate key (private address page offset +0x10,
   per resume864) and target `ThreadId` are captured and how the qperf-only
   tag reaches request publication. Avoid adding a production contract or
   changing wake order/fences.
2. The proposed per-CPU seqlock claimed a single writer, but a gate wake
   can be interrupted/preempted before slot publication and another task
   may write the same CPU slot. Either prove single-writer for this exact
   stage, use a sound multi-writer atomic state machine, or move the slot
   to a lifetime-owned per-target object. Specify overwrite/failed-claim
   accounting and memory order.
3. A0->publish is a distinct possible multi-us segment. Include it in
   the decomposition and self-check. `sys_clock_gettime` from the sender
   may serve other calls before/after A0; explain how 'last clock before
   gate' and 'first target clock after switch' are verified or rejected,
   not assumed. The frozen binary does not expose its raw pairs to the
   host; explain what can and cannot be cross-checked against its mean.
4. One failed/invalid `not_parked` attempt may still have a matching
   gate wake and clock read; account for this before matching nth wake
   to nth A5. Multiple wake branches, same-offset unrelated futexes,
   background syscalls and migration must have explicit exclusion or
   loss counters.
5. The previous answer claimed a 4-6-clock perturbation and extrapolated
   from clock_pair_min=583ns. This is not a measured probe cost; remove
   the numeric inference or substantiate it precisely.

Keep the response focused on a single minimal diagnostic protocol and
its implementation boundaries. No native speedup claims.
