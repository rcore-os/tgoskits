/* Hand-written contract probe: wait4(2) pid not a child -> ECHILD. */
#include <errno.h>
#include <stdio.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void)
{
	errno = 0;
	pid_t r = wait4(999999, NULL, 0, NULL);
	int e = errno;
	dprintf(1, "CASE wait4.nochld ret=%d errno=%d note=handwritten\n", (int)r, e);
	return 0;
}
