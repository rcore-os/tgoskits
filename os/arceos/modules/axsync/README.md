# ax-sync

OS-independent synchronization interfaces for TGOSKits kernels and reusable
components.

## Primitives

- `SpinLock<T>` and `SpinRwLock<T>` select execution-context policy at each
  acquisition: ordinary methods disable preemption, `*_irqsave` methods also
  save and disable local interrupts, and `unsafe *_raw` methods leave context
  management to the caller.
- `Mutex<T>` is always a non-poisoning sleepable mutex. It is available only
  with the `sleep` feature and never aliases a spin lock.
- `PreemptGuard`, `IrqSaveGuard`, and `PreemptIrqSaveGuard` provide explicit
  critical-section guards.
- With `lockdep`, all lock types share lock-class, held-lock, ordering, and
  diagnostic support.

The crate declares runtime capabilities through `ax-crate-interface`.
ArceOS implements the production providers in `ax-runtime`; host tests use the
`host-test` providers in this crate.

## Features

- `smp`: enable atomic multi-CPU exclusion.
- `sleep`: enable the sleepable mutex interface.
- `lockdep`: enable held-lock and ordering diagnostics.
- `lock-api`: enable the IRQ-save raw mutex adapter required by `lock_api`.
- `host-test`: use the deterministic host-side engine on non-bare-metal targets.
- `axtest`: expose bare-metal coverage tests.

## License

This project is licensed under GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0.
