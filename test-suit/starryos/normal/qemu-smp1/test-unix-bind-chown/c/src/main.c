/*
 * test-unix-bind-chown
 *
 * Verifies that an AF_UNIX pathname socket created by bind(2) is owned by
 * the binding process's fsuid/fsgid, matching Linux semantics.
 *
 * Bug: sys_bind created the socket file without a credential context, so the
 * inode owner was always root (uid 0 / gid 0). A non-root process could not
 * then fchmodat its own socket -- EPERM from VFS uid check.
 *
 * Test procedure:
 *   Fork a child that drops to uid/gid 1000 via setresuid/setresgid, then
 *   bind an AF_UNIX path socket at a known path, and exits without unlinking.
 *   The parent (still uid 0) stats the socket file and verifies st_uid == 1000.
 */

#include "test_framework.h"
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#define SOCK_PATH "/tmp/test-b19-bind.sock"
#define TEST_UID  1000
#define TEST_GID  1000

int main(void)
{
    TEST_START("unix bind(2) chowns socket to caller fsuid/fsgid");

    unlink(SOCK_PATH);

    pid_t pid = fork();
    CHECK(pid >= 0, "fork");
    if (pid == 0) {
        if (setresgid(TEST_GID, TEST_GID, TEST_GID) != 0)
            _exit(2);
        if (setresuid(TEST_UID, TEST_UID, TEST_UID) != 0)
            _exit(3);

        int sock = socket(AF_UNIX, SOCK_STREAM, 0);
        if (sock < 0)
            _exit(4);

        struct sockaddr_un addr = {0};
        addr.sun_family = AF_UNIX;
        __builtin_strncpy(addr.sun_path, SOCK_PATH, sizeof(addr.sun_path) - 1);

        if (bind(sock, (struct sockaddr *)&addr, sizeof(addr)) != 0)
            _exit(5);

        close(sock);
        _exit(0);
    }

    int status = 0;
    pid_t w;
    do {
        w = waitpid(pid, &status, 0);
    } while (w == -1 && errno == EINTR);
    CHECK(w == pid, "waitpid child");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "child bind succeeded");

    struct stat st;
    int sr = stat(SOCK_PATH, &st);
    CHECK(sr == 0, "stat socket path");
    if (sr == 0) {
        CHECK((st.st_mode & S_IFMT) == S_IFSOCK, "inode is a socket");
        CHECK((uid_t)st.st_uid == (uid_t)TEST_UID,
              "socket owner uid == TEST_UID (1000)");
        CHECK((gid_t)st.st_gid == (gid_t)TEST_GID,
              "socket owner gid == TEST_GID (1000)");
    }

    unlink(SOCK_PATH);

    TEST_DONE();
}
