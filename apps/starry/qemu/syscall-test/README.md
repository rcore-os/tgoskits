# Starry syscall-test app

This app ports the LTP syscall runner from the `ltp-test` branch into
`apps/starry/qemu/syscall-test`.

The runner executes syscall case lists against LTP binaries already available in
the guest rootfs. By default it expects:

```text
/opt/ltp/runtest/syscalls
/opt/ltp/testcases/bin/*
```

The app prebuild installs:

```text
/usr/bin/syscall-test
/usr/bin/starry-test-suit/syscall
/usr/share/starry-test-suit/syscall/TODO.txt
/usr/share/starry-test-suit/syscall/syscalls/*.txt
```

Run the default app for an architecture:

```bash
cargo xtask starry app qemu -t qemu/syscall-test --arch x86_64
cargo xtask starry app qemu -t qemu/syscall-test --arch riscv64
```

This app includes `RUN.txt` next to this README to limit local smoke runs to a
small set of syscall groups:

```text
close
getegid
gettimeofday
uname
```

Remove `RUN.txt` locally when running the full syscall suite. Entries in
`TODO.txt` always take precedence and are skipped even if listed in `RUN.txt`.

Source provenance:

- LTP release: `20260529`
- Upstream syscall cases: <https://github.com/linux-test-project/ltp/tree/20260529/testcases/kernel/syscalls>
- Upstream syscall runtest list: <https://github.com/linux-test-project/ltp/blob/20260529/runtest/syscalls>
