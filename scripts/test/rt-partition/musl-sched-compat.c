#include <sched.h>
#include <sys/syscall.h>
#include <unistd.h>

/*
 * The GCC 11 aarch64-musl toolchain used by the Task1 experiment ships
 * ENOSYS stubs for these three libc entry points even though the kernel ABI
 * provides the corresponding syscalls.  rt-tests uses all three during its
 * privilege probe, so provide the thin wrappers locally.
 */
int sched_getparam(pid_t pid, struct sched_param *param)
{
	return (int)syscall(SYS_sched_getparam, pid, param);
}

int sched_getscheduler(pid_t pid)
{
	return (int)syscall(SYS_sched_getscheduler, pid);
}

int sched_setscheduler(pid_t pid, int policy, const struct sched_param *param)
{
	return (int)syscall(SYS_sched_setscheduler, pid, policy, param);
}
