#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PR_SET_NO_NEW_PRIVS
#define PR_SET_NO_NEW_PRIVS 38
#endif
#ifndef PR_GET_NO_NEW_PRIVS
#define PR_GET_NO_NEW_PRIVS 39
#endif

#define ASSERT(expr, msg) do { \
    if (!(expr)) { \
        printf("  FAIL | %s:%d | %s\n", __FILE__, __LINE__, msg); \
        exit(1); \
    } \
    printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg); \
} while (0)

int main(void) {
    int ret;

    printf("=== test PR_GET_NO_NEW_PRIVS (initial) ===\n");
    ret = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0);
    ASSERT(ret == 0, "initially no_new_privs is 0");

    printf("=== test PR_SET_NO_NEW_PRIVS ===\n");
    ret = prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    ASSERT(ret == 0, "set no_new_privs returns 0");

    printf("=== test PR_GET_NO_NEW_PRIVS (after set) ===\n");
    ret = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0);
    ASSERT(ret == 1, "after set, no_new_privs is 1");

    printf("=== test invalid args ===\n");
    ret = prctl(PR_SET_NO_NEW_PRIVS, 0, 0, 0, 0);
    ASSERT(ret == -1 && errno == EINVAL, "arg2=0 returns EINVAL");

    ret = prctl(PR_SET_NO_NEW_PRIVS, 1, 1, 0, 0);
    ASSERT(ret == -1 && errno == EINVAL, "arg3!=0 returns EINVAL");

    printf("=== test child inherits no_new_privs ===\n");
    pid_t pid = fork();
    if (pid == 0) {
        ret = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0);
        if (ret != 1) {
            printf("  FAIL | %s:%d | child did not inherit no_new_privs\n", __FILE__, __LINE__);
            _exit(1);
        }
        printf("  PASS | %s:%d | child inherits no_new_privs\n", __FILE__, __LINE__);
        _exit(0);
    }
    int status;
    waitpid(pid, &status, 0);
    ASSERT(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child exited successfully");

    printf("ALL PASSED\n");
    return 0;
}
