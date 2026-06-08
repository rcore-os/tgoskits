# StarryOS GDB Smoke

This app prepares a RISC-V Alpine rootfs overlay with guest `gdb`, `gdbserver`,
and tiny target programs for StarryOS user-space debugger smoke testing.

## Batch Native GDB Smoke

Use this command for the automated native GDB batch smoke:

```bash
cargo xtask starry app qemu -t gdb-smoke --arch riscv64
```

The batch script runs:

```gdb
break native_marker
run
bt
info proc mappings
info files
info auxv
shell pid="$(pidof gdb-native-smoke-target)" && cat "/proc/$pid/status"
info registers
x/4gx $sp
stepi
continue
```

Success requires all `GDB_NATIVE_*` markers from
`native/gdb-native-smoke.gdb`.

## Manual Native GDB Demo

Use this entry when you want an interactive StarryOS shell:

```bash
cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-manual.toml
```

When running through the long-lived Docker container, keep stdin and a TTY
attached:

```bash
docker exec -it tgoskits-dev cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-manual.toml
```

This keeps QEMU's serial console interactive inside Docker. Use `Ctrl+A`, then
`x`, to leave the QEMU console after the manual demo.

Inside StarryOS:

```bash
gdb /usr/bin/gdb-native-smoke-target
```

This starts the guest-side GDB and loads symbols for the native smoke target.

Inside GDB:

```gdb
break native_marker
run
bt
info registers
stepi
continue
quit
```

These commands set a breakpoint, run to it, print a backtrace and registers,
single-step once, then continue the target to normal exit.

The native target uses a clear call chain:

```text
main -> demo_entry -> demo_worker -> native_marker
```

## Native Thread GDB Smoke

Use this command for the native GDB thread smoke:

```bash
cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-threads.toml
```

The thread script enables GDB's multi-thread scheduling, breaks on
`thread_marker`, runs `info threads`, lists `/proc/<pid>/task`, prints a
backtrace, deletes the breakpoint, and continues the target to normal exit.

## GDBServer Smoke

Use this command for the guest-internal gdbserver smoke:

```bash
cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-gdbserver.toml
```

The default gdbserver script connects to `127.0.0.1:1234`, breaks on
`compute_value`, prints a backtrace, deletes the breakpoint, and continues the
remote target.

Remote pthread gdbserver coverage is opt-in because it is slower and exercises
the heavier clone/thread event path:

```bash
cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-gdbserver-debug.toml
```

Set `GDBSERVER_SMOKE_SERVER_DEBUG=1` in the QEMU config when gdbserver's own
debug trace is needed for a focused investigation.

## GDB Stress

Use this opt-in entry for the heavier ptrace/GDB stress path:

```bash
cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-stress.toml
```

This stress target exercises a multi-threaded tracee with clone events, a
software breakpoint written through `/proc/<pid>/mem`, register access,
single-step, and delayed tracer scheduling. It is intentionally kept out of the
default batch smoke and remote CI paths.

## Host-To-Guest Remote GDB Demo

Use this entry when you want the host to connect to guest `gdbserver` through
QEMU user-network port forwarding:

```bash
cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-gdbserver-manual.toml
```

When running through the long-lived Docker container, keep stdin and a TTY
attached:

```bash
docker exec -it tgoskits-dev cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-gdbserver-manual.toml
```

This manual config keeps the guest shell open and forwards host TCP port 1234 to
guest TCP port 1234.

Inside StarryOS, start `gdbserver` and leave it waiting for the host GDB:

```bash
gdbserver 0.0.0.0:1234 /usr/bin/gdbserver-smoke-target
```

`0.0.0.0:1234` is required for the QEMU host-forwarded connection. The command
blocks until the host GDB connects.

On the host side, use the copied symbol file produced by `prebuild.sh` and keep
GDB interactive:

```bash
gdb-multiarch -q -x apps/starry/gdb-smoke/gdbserver/host-manual.gdb \
  target/gdb-smoke-host/gdbserver-smoke-target
```

`host-manual.gdb` sets the riscv64 remote debugging defaults and connects to
`:1234`, but leaves you at the GDB prompt for manual commands.

Inside host GDB:

```gdb
break compute_value
continue
bt
info registers
detach
quit
```

These commands prove host-to-guest remote debugging: insert a breakpoint in the
guest process, continue to it, inspect stack/registers, then detach cleanly so
the guest target can finish.

For the reproducible host-to-guest batch demo, start the guest-side
`gdbserver` with:

```bash
cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-gdbserver-host.toml
```

This automatic config starts guest `gdbserver` for you and is intended for
repeatable logs rather than manual interaction.

Then run the batch host script:

```bash
gdb-multiarch -q -batch -x apps/starry/gdb-smoke/gdbserver/host-remote.gdb \
  target/gdb-smoke-host/gdbserver-smoke-target
```

`-batch` runs the scripted host GDB flow and exits after the marker output.
