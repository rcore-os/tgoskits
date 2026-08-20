# axpoll-set

`axpoll-set` is the common `no_std` readiness queue implementation for TGOSKits.
It stores owned registrations defined by the pure `axpoll` API and applies Linux
waitqueue selection semantics: one wake notifies every matching shared observer
and at most one matching exclusive consumer.

Registration and wake selection run in task or deferred context. Hard interrupt
handlers publish readiness and use the runtime's typed IRQ-to-task bridge before
the deferred service calls `PollSet::wake`.
