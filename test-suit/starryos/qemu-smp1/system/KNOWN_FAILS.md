# qemu-smp1/system Known-Fail Probes

These probes are still built and installed into `/usr/bin/starry-known-fail`,
but the grouped CI runner only executes `/usr/bin/starry-test-suit/*`.

- `test-ebpf-basics`: positive `bpf(2)` map/program operations require `starry-kernel/ebpf-kmod`.
- `test-ebpf-advanced`: `BPF_OBJ_CLOSE` map/program fd semantics.
- `test-ebpf-attach`: perf kprobe plus BPF attach/link semantics.
- `test-epoll-eventfd`: `EPOLLET` eventfd wakeup consumption.
- `test-epoll-network`: `EPOLLET` socket wakeup consumption.
- `test-io-getevents`: Linux AIO negative `nr` errno precedence.
- `test-io-submit`: Linux AIO negative `nr` errno precedence.
- `test-ioctl`: termios mutation through ioctl.
- `test-mt-execve`: `execve(path, NULL, NULL)` argv/envp behavior.
- `test-open-family`: strict open/openat matrix currently reaches known Starry gaps.
- `test-ptrace-exec-stop`: ptrace exec-stop `SIGTRAP` semantics.
- `test-preadv-pwritev2`: `O_APPEND` with `pwritev`/`pwritev2`.
- `test-sigqueueinfo`: queued signal delivery with `siginfo`.
- `test-sigtimedwait`: queued signal delivery with `siginfo`.
- `test-splice`: offset and same-pipe edge cases.
- `test-sync-file-range`: errno precedence for invalid fd plus invalid flags.
- `test-tgsigqueueinfo`: queued thread signal delivery with `siginfo`.
- `test-uid-gid-direct-setters`: uid/gid setter boundary matrix semantics.
- `test-uid-gid-groups`: user namespace `setgroups=deny` behavior.
- `test-uid-gid-res-setters`: setresuid/setresgid boundary matrix semantics.
