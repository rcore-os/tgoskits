# cgroup v2 testdata

Real-world cgroup v2 interface-file samples captured from a Linux host
(`/sys/fs/cgroup/...`). M1 round-trip tests parse these and compare the
re-serialized output field-by-field, so the crate's format matches Linux.

Keep each file byte-faithful to the kernel output (including the trailing
newline) — the tests assert on exact formatting.
