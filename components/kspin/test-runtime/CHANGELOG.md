# Changelog

## Unreleased

- Track IRQ and preemption nesting independently for each host test thread.
- Expose snapshots and resets for consumer tests while keeping the provider an
  explicitly linked test fixture.
- Assign stable nonzero host thread identifiers and reject nesting arithmetic
  underflow or overflow.
