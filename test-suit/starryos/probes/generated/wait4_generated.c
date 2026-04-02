/* GENERATED — wait4 — template contract_wait4_echild */
#include <errno.h>
#include <stdio.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
  errno = 0;
  pid_t r = wait4(999999, NULL, 0, NULL);
  int e = errno;
  dprintf(1, "CASE wait4.nochld ret=%d errno=%d note=generated-from-catalog\n", (int)r, e);
  return 0;
}
