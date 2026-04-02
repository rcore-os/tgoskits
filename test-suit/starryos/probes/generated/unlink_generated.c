/* GENERATED — unlink — template contract_unlink_enoent */
#include <errno.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
  errno = 0;
  int r = unlink("/__starryos_probe_unlink__/not_there");
  int e = errno;
  dprintf(1, "CASE unlink.enoent ret=%d errno=%d note=generated-from-catalog\n", r, e);
  return 0;
}
