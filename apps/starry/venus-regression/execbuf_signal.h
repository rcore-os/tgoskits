#ifndef VENUS_EXECBUF_SIGNAL_H
#define VENUS_EXECBUF_SIGNAL_H

/* Exercises the userspace EXECBUF input-fence wait and the StarryOS
 * interruptible-syscall / SA_RESTART path against a pending KMS out-fence
 * whose vblank completion is paced by the test's low-clock mode. Returns 0
 * on success; on failure it prints VENUS_REGRESSION_FAILED and exits. */
int execbuf_signal_test(void);

#endif
