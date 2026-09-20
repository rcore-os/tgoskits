# qemu/system Known-Fail Probes

These probes are still built and installed into `/usr/bin/starry-known-fail`,
but the grouped CI runner only executes `/usr/bin/starry-test-suit/*`.

- `test-ebpf-attach`: perf kprobe plus BPF attach/link semantics.
- `test-io-getevents`: Linux AIO negative `nr` errno precedence.
- `test-io-submit`: Linux AIO negative `nr` errno precedence.
- `test-ioctl`: termios mutation through ioctl.
- `test-mt-execve`: `execve(path, NULL, NULL)` argv/envp behavior.
- `test-ptrace-exec-stop`: ptrace exec-stop `SIGTRAP` semantics.
- `test-sigqueueinfo`: queued signal delivery with `siginfo`.
- `test-sigtimedwait`: queued signal delivery with `siginfo`.
