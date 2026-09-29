# resume863: Fair wake / preempt-exit read-only audit

On `b292a098bb60ef604e7677c37cd95d926ff08200`, the independent
read-only audit found no same-transaction Fair virtual-time, rq commit,
selection or deadline publication operation that can be safely removed.
The two Fair virtual-time updates bracket wakee insertion; the wake and
scheduling `settle_current` calls sample different rq transactions; and
the schedule transaction can reinsert the sender before final selection.

The audit also challenged a possible interpretation of resume862:
`WakePreemptionDecision::WakeeSelected` maps Fair to Lazy, while
`publish_rq_scheduler_reasons` arms local scheduler work only for
`owner_work` or Immediate. I checked both source conditions directly.
The aggregate `context_switches_preempted` delta therefore does not locate
the switch inside `wake_batch`; other pending work or later Lazy service
remain possible. The archived probe has no per-event pairing, so it cannot
decide which path dominates. The stage span is not a removable native cost.

Decision: diagnostic only. No source candidate, build, board run or native
performance claim. Keep resume862's five valid rounds and raw logs as
branch/path evidence; a future targeted experiment would have to pair
switches with individual wake windows before optimizing this handoff.
