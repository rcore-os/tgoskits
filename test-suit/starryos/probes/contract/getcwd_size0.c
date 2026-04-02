/* Hand-written contract probe: getcwd(3) with non-NULL buf and size 0 -> EINVAL. */
#include <errno.h>
#include <stdio.h>
#include <unistd.h>

int main(void)
{
	char buf[4];

	errno = 0;
	char *r = getcwd(buf, 0);
	int e = errno;
	dprintf(1, "CASE getcwd.size_zero ret=%d errno=%d note=handwritten\n", r ? 0 : -1, e);
	return 0;
}
