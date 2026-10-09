#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/file.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed;
static int failed;

static void check(int condition, const char *message)
{
    if (condition) {
        ++passed;
        printf("PASS: %s\n", message);
    } else {
        ++failed;
        printf("FAIL: %s\n", message);
    }
}

static const char *path = "/tmp/flock-last-close";

/* A second open file description on the same inode contends with the lock,
 * which is how another process would see it. */
static int contended(void)
{
    int probe = open(path, O_RDWR);
    if (probe < 0) {
        return -1;
    }
    int result = flock(probe, LOCK_EX | LOCK_NB);
    int saved = errno;
    close(probe);
    if (result == 0) {
        return 0;
    }
    return saved == EWOULDBLOCK ? 1 : -1;
}

int main(void)
{
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
    check(fd >= 0, "create the lock file");
    if (fd < 0) {
        printf("RESULT: %d passed / %d failed\nTEST FAILED\n", passed, failed);
        return 1;
    }

    /* Only descriptor: closing it is the last reference. */
    check(flock(fd, LOCK_EX) == 0, "take an exclusive flock");
    check(contended() == 1, "another description sees the lock");
    close(fd);
    check(contended() == 0, "closing the only descriptor releases the lock");

    /* dup shares the description, so one close must not release it. */
    fd = open(path, O_RDWR);
    int copy = dup(fd);
    check(copy >= 0 && flock(fd, LOCK_EX) == 0, "lock through one of two descriptors");
    close(fd);
    check(contended() == 1, "the lock survives while a dup still refers to it");
    close(copy);
    check(contended() == 0, "closing the last dup releases the lock");

    /* A forked child shares the description through its own table. */
    fd = open(path, O_RDWR);
    check(flock(fd, LOCK_EX) == 0, "lock before fork");
    int to_child[2];
    int to_parent[2];
    check(pipe(to_child) == 0 && pipe(to_parent) == 0, "create the handshake pipes");
    pid_t child = fork();
    if (child == 0) {
        char byte;
        close(to_child[1]);
        close(to_parent[0]);
        /* Hold the inherited descriptor until the parent has closed its own. */
        if (read(to_child[0], &byte, 1) != 1) {
            _exit(2);
        }
        int held = contended();
        close(fd);
        int released = contended();
        char verdict = (held == 1 && released == 0) ? 'y' : 'n';
        if (write(to_parent[1], &verdict, 1) != 1) {
            _exit(3);
        }
        _exit(0);
    }
    close(to_child[0]);
    close(to_parent[1]);
    close(fd);
    check(contended() == 1, "the lock survives the parent close while the child holds it");
    char byte = 'g';
    char verdict = 0;
    check(write(to_child[1], &byte, 1) == 1, "let the child close its copy");
    check(read(to_parent[0], &verdict, 1) == 1 && verdict == 'y',
          "the child still sees the lock and its close releases it");
    int status = 0;
    waitpid(child, &status, 0);
    check(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the child exits cleanly");
    check(contended() == 0, "no lock remains after both closes");

    unlink(path);
    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
