/* Hand-written contract: read(2) stdin count=0 + write(2) stdout len=0 in one ELF (multi-CASE). */
#include <errno.h>
#include <stdio.h>
#include <unistd.h>

int main(void)
{
	errno = 0;
	ssize_t n1 = read(0, NULL, 0);
	int e1 = errno;
	dprintf(1, "CASE io_zero_rw.read_stdin_count0 ret=%zd errno=%d note=handwritten\n", n1, e1);

	errno = 0;
	ssize_t n2 = write(1, "", 0);
	int e2 = errno;
	dprintf(1, "CASE io_zero_rw.write_stdout_count0 ret=%zd errno=%d note=handwritten\n", n2, e2);
	return 0;
}
