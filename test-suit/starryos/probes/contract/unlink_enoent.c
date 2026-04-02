/* Hand-written contract probe: unlink(2) nonexistent absolute path -> ENOENT. */
#include <errno.h>
#include <stdio.h>
#include <unistd.h>

int main(void)
{
	errno = 0;
	int r = unlink("/__starryos_probe_unlink__/not_there");
	int e = errno;
	dprintf(1, "CASE unlink.enoent ret=%d errno=%d note=handwritten\n", r, e);
	return 0;
}
