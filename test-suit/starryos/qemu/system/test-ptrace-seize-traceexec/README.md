# PTRACE_SEIZE TRACEEXEC Regression

This x86_64 QEMU regression covers equivalent `PTRACE_SEIZE` and
`PTRACE_TRACEME` sequences for a traced `execve` after an initial job-control
stop:

1. A `PTRACE_SEIZE` tracer interrupts a tracee stopped by `SIGSTOP`.
   `waitpid()` must report `SIGSTOP | (PTRACE_EVENT_STOP << 8)`.
2. With `PTRACE_O_TRACESYSGOOD` and `PTRACE_SYSCALL`, the `execve` entry stop
   must expose `orig_rax = SYS_execve` and `rax = -ENOSYS`.
3. With `PTRACE_O_TRACEEXEC`, resuming that entry stop must report
   `SIGTRAP | (PTRACE_EVENT_EXEC << 8)`, not an `execve` syscall-exit stop.
4. After that event, `PTRACE_SYSCALL` and `PTRACE_GETREGSET` must keep
   `orig_rax` at `SYS_getppid` from its entry stop through its exit stop, while
   `rax` changes from `-ENOSYS` to the tracer PID.
5. A `PTRACE_TRACEME` tracee configured with the same options must observe the
   same post-exec `SYS_getppid` register pair, then preserve `orig_rax` and the
   return value through a direct `SYS_getpid` entry and exit stop.

The public ptrace event and option contract is documented by
[ptrace(2)](https://man7.org/linux/man-pages/man2/ptrace.2.html).
The precise stop sequence and x86_64 register observation above were checked
by running this source directly against Linux `7.1.5-zen1` on 2026-08-05:

```text
PTRACE_STOP: PTRACE_INTERRUPT reports PTRACE_EVENT_STOP status=0x80137f
PTRACE_SYSCALL_REGS: orig_rax=59 rax=-38
PTRACE_STOP: PTRACE_O_TRACEEXEC replaces execve syscall-exit stop status=0x4057f
PTRACE_POST_EXEC_REGS: orig_rax=110 rax=-38
PTRACE_POST_EXEC_REGS: orig_rax=110 rax=<tracer-pid>
PTRACE_TRACEME_POST_EXEC_REGS: orig_rax=110 rax=-38
PTRACE_TRACEME_POST_EXEC_REGS: orig_rax=110 rax=<tracer-pid>
PTRACE_TRACEME_POST_EXEC_REGS: orig_rax=39 rax=-38
PTRACE_TRACEME_POST_EXEC_REGS: orig_rax=39 rax=<tracee-pid>
```

In StarryOS, the behavior spans the ptrace request handlers, wait-status
translation, job-stop wakeup path, and per-thread syscall trace state. Keep the
test focused on that observable ABI rather than on an internal stop ordering.
