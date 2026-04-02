/* GENERATED — execve — template contract_execve_enoent */
#include <errno.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
  char *argv[] = { "/__starryos_probe__/execve_no_such", NULL };
  char *envp[] = { NULL };
  errno = 0;
  int r = execve("/__starryos_probe__/execve_no_such", argv, envp);
  int e = errno;
  dprintf(1, "CASE execve.enoent ret=%d errno=%d note=generated-from-catalog\n", r, e);
  return 0;
}
