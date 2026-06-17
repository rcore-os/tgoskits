/*
 * bug-redis-aof-appendonly: redis-server with AOF enabled must accept local
 * redis-cli PING (PONG).
 *
 * Diagnosis (2026-05-18, QEMU riscv64 Alpine, Redis 8.4.2):
 *   redis-server --appendonly yes fails during AOF init with:
 *     "Error moving temp append only file on the final destination: Invalid argument"
 *   fg_exit=1; redis-cli -> Connection refused.
 *   Control without AOF on another port still PONGs.
 *
 * Likely root cause: Redis moves a temp AOF file onto the final name via
 * rename(2) (replace existing). StarryOS has known rename-replace issues;
 * see bugfix/bug-rename-replace and sys_renameat2 in kernel syscall/fs/ctl.rs.
 *
 * Do NOT add /usr/bin/bug-redis-aof-appendonly to bugfix/qemu-*.toml
 * test_commands until this test passes (normal CI runs the full bugfix group).
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int run(const char *cmd)
{
    int rc = system(cmd);
    if (rc == -1) {
        perror("system");
        return -1;
    }
    if (!WIFEXITED(rc) || WEXITSTATUS(rc) != 0) {
        printf("  FAIL: command exited non-zero: %s (status=%d)\n", cmd, rc);
        return -1;
    }
    return 0;
}

int main(void)
{
    printf("=== bug-redis-aof-appendonly ===\n");

    if (run("redis-server --daemonize yes --port 6379 --bind 127.0.0.1 "
            "--save \"\" --appendonly yes --appendfilename appendonly.aof") != 0) {
        return 1;
    }
    sleep(2);

    FILE *fp = popen("redis-cli ping 2>&1", "r");
    if (!fp) {
        perror("popen");
        return 1;
    }
    char line[128];
    if (!fgets(line, sizeof(line), fp)) {
        printf("  FAIL: redis-cli ping produced no output\n");
        pclose(fp);
        return 1;
    }
    pclose(fp);

    if (strncmp(line, "PONG", 4) != 0) {
        printf("  FAIL: expected PONG, got: %s", line);
        return 1;
    }
    printf("  PASS: redis-cli ping -> PONG\n");
    run("redis-cli shutdown nosave 2>/dev/null || true");
    return 0;
}
