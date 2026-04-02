/* GENERATED — getcwd — template contract_getcwd_size0 */
#include <errno.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
  char buf[4];
  errno = 0;
  char *r = getcwd(buf, 0);
  int e = errno;
  dprintf(1, "CASE getcwd.size_zero ret=%d errno=%d note=generated-from-catalog\n", r ? 0 : -1, e);
  return 0;
}
