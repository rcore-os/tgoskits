/*
 * test-gcc.c -- GCC compilation validation for StarryOS
 *
 * Key dependencies: fork, execve, pipe, waitpid, mmap, file locking
 * Acceptance criteria:
 *   - Compile and run hello.c with gcc
 *   - Compile a multi-file C project
 *   - Compiled programs use fork/execve/pipe/waitpid correctly
 *   - Compiled programs use mmap correctly
 *   - Compiled programs use flock/fcntl file locking correctly
 */

#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int pass = 0, fail = 0;

/* Run a shell command via system(), return exit status (0-255) */
static int run(const char *cmd)
{
    int ret = system(cmd);
    if (WIFEXITED(ret))
        return WEXITSTATUS(ret);
    return -1;
}

/* Write data to a file, return 0 on success */
static int write_file(const char *path, const char *data)
{
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0)
        return -1;
    size_t len = strlen(data);
    ssize_t w = write(fd, data, len);
    close(fd);
    return (w == (ssize_t)len) ? 0 : -1;
}

/*
 * Capture first line of a command's stdout into buf via popen().
 * Returns exit status (0-255) or -1 on error.
 */
static int capture(const char *cmd, char *buf, int bufsz)
{
    FILE *p = popen(cmd, "r");
    if (!p)
        return -1;
    buf[0] = '\0';
    if (!fgets(buf, bufsz, p)) {
        pclose(p);
        buf[0] = '\0';
        return -1;
    }
    int status = pclose(p);
    /* Strip trailing newlines */
    int len = (int)strlen(buf);
    while (len > 0 && (buf[len - 1] == '\n' || buf[len - 1] == '\r'))
        buf[--len] = '\0';
    if (WIFEXITED(status))
        return WEXITSTATUS(status);
    return -1;
}

#define PASS(name)                                                             \
    do {                                                                       \
        printf("  PASS | %s\n", name);                                        \
        pass++;                                                                \
    } while (0)
#define FAIL(name, ...)                                                        \
    do {                                                                       \
        printf("  FAIL | %s ", name);                                         \
        printf(__VA_ARGS__);                                                   \
        printf("\n");                                                          \
        fail++;                                                                \
    } while (0)

/* ==================================================================
 *  Group 1: Tool availability
 * ================================================================== */
static void test_tool_availability(void)
{
    printf("[tool availability]\n");

    /* 1. gcc --version */
    {
        char buf[256];
        int rc = capture("gcc --version 2>&1", buf, sizeof(buf));
        if (rc == 0 && strlen(buf) > 0) {
            PASS("gcc available");
        } else {
            FAIL("gcc available", "(rc=%d output='%s')", rc, buf);
        }
    }

    /* 2. ld available */
    {
        int rc = run("which ld >/dev/null 2>&1");
        if (rc == 0) {
            PASS("ld available");
        } else {
            FAIL("ld available", "(rc=%d)", rc);
        }
    }
}

/* ==================================================================
 *  Group 2: Simple C compilation — hello.c
 * ================================================================== */
static void test_hello_c(void)
{
    printf("[hello.c]\n");

    /* 1. Write hello.c */
    {
        int rc = write_file("/tmp/hello.c",
                            "#include <stdio.h>\n"
                            "int main(void) {\n"
                            "    printf(\"Hello, World!\\n\");\n"
                            "    return 0;\n"
                            "}\n");
        if (rc == 0) {
            PASS("write hello.c");
        } else {
            FAIL("write hello.c", "(errno=%d)", errno);
        }
    }

    /* 2. Compile hello.c */
    {
        int rc = run("gcc -o /tmp/hello /tmp/hello.c 2>&1");
        if (rc == 0) {
            PASS("gcc compile hello.c");
        } else {
            FAIL("gcc compile hello.c", "(rc=%d)", rc);
        }
    }

    /* 3. Run hello and check output */
    {
        char buf[256];
        int rc = capture("/tmp/hello 2>&1", buf, sizeof(buf));
        if (rc == 0 && strcmp(buf, "Hello, World!") == 0) {
            PASS("run hello");
        } else {
            FAIL("run hello", "(rc=%d output='%s')", rc, buf);
        }
    }
}

/* ==================================================================
 *  Group 3: Multi-file C project
 * ================================================================== */
static void test_multi_file(void)
{
    printf("[multi-file C]\n");

    run("mkdir -p /tmp/multi");

    /* 1. Write greet.h */
    {
        int rc = write_file("/tmp/multi/greet.h",
                            "#ifndef GREET_H\n"
                            "#define GREET_H\n"
                            "const char *get_greeting(void);\n"
                            "#endif\n");
        if (rc == 0) {
            PASS("write greet.h");
        } else {
            FAIL("write greet.h", "(errno=%d)", errno);
        }
    }

    /* 2. Write greet.c */
    {
        int rc = write_file("/tmp/multi/greet.c",
                            "#include \"greet.h\"\n"
                            "const char *get_greeting(void) {\n"
                            "    return \"Hello from multi-file!\";\n"
                            "}\n");
        if (rc == 0) {
            PASS("write greet.c");
        } else {
            FAIL("write greet.c", "(errno=%d)", errno);
        }
    }

    /* 3. Write main.c */
    {
        int rc = write_file("/tmp/multi/main.c",
                            "#include <stdio.h>\n"
                            "#include \"greet.h\"\n"
                            "int main(void) {\n"
                            "    printf(\"%s\\n\", get_greeting());\n"
                            "    return 0;\n"
                            "}\n");
        if (rc == 0) {
            PASS("write multi/main.c");
        } else {
            FAIL("write multi/main.c", "(errno=%d)", errno);
        }
    }

    /* 4. Compile multi-file project */
    {
        int rc =
            run("gcc -o /tmp/multi_prog /tmp/multi/main.c /tmp/multi/greet.c "
                "-I/tmp/multi 2>&1");
        if (rc == 0) {
            PASS("gcc compile multi-file");
        } else {
            FAIL("gcc compile multi-file", "(rc=%d)", rc);
        }
    }

    /* 5. Run multi-file program */
    {
        char buf[256];
        int rc = capture("/tmp/multi_prog 2>&1", buf, sizeof(buf));
        if (rc == 0 && strcmp(buf, "Hello from multi-file!") == 0) {
            PASS("run multi_prog");
        } else {
            FAIL("run multi_prog", "(rc=%d output='%s')", rc, buf);
        }
    }
}

/* ==================================================================
 *  Group 4: fork + pipe + waitpid
 *
 *  Compile a program that forks a child, sends a message through a
 *  pipe, and the parent reads it back via waitpid.
 * ================================================================== */
static void test_fork_pipe_waitpid(void)
{
    printf("[fork/pipe/waitpid]\n");

    /* 1. Write fork_pipe_test.c */
    {
        int rc = write_file("/tmp/fork_pipe_test.c",
                            "#include <stdio.h>\n"
                            "#include <stdlib.h>\n"
                            "#include <string.h>\n"
                            "#include <unistd.h>\n"
                            "#include <sys/wait.h>\n"
                            "\n"
                            "int main(void) {\n"
                            "    int pipefd[2];\n"
                            "    if (pipe(pipefd) < 0) return 1;\n"
                            "    pid_t pid = fork();\n"
                            "    if (pid < 0) return 1;\n"
                            "    if (pid == 0) {\n"
                            "        close(pipefd[0]);\n"
                            "        const char *msg = \"pipe-works\";\n"
                            "        write(pipefd[1], msg, strlen(msg));\n"
                            "        close(pipefd[1]);\n"
                            "        _exit(0);\n"
                            "    }\n"
                            "    close(pipefd[1]);\n"
                            "    char buf[64] = {0};\n"
                            "    read(pipefd[0], buf, sizeof(buf) - 1);\n"
                            "    close(pipefd[0]);\n"
                            "    int status;\n"
                            "    waitpid(pid, &status, 0);\n"
                            "    printf(\"%s\\n\", buf);\n"
                            "    return (WIFEXITED(status) && WEXITSTATUS(status) == 0\n"
                            "            && strcmp(buf, \"pipe-works\") == 0) ? 0 : 1;\n"
                            "}\n");
        if (rc == 0) {
            PASS("write fork_pipe_test.c");
        } else {
            FAIL("write fork_pipe_test.c", "(errno=%d)", errno);
        }
    }

    /* 2. Compile */
    {
        int rc = run("gcc -o /tmp/fork_pipe_test /tmp/fork_pipe_test.c 2>&1");
        if (rc == 0) {
            PASS("gcc compile fork_pipe_test.c");
        } else {
            FAIL("gcc compile fork_pipe_test.c", "(rc=%d)", rc);
        }
    }

    /* 3. Run */
    {
        char buf[256];
        int rc = capture("/tmp/fork_pipe_test 2>&1", buf, sizeof(buf));
        if (rc == 0 && strcmp(buf, "pipe-works") == 0) {
            PASS("run fork_pipe_test");
        } else {
            FAIL("run fork_pipe_test", "(rc=%d output='%s')", rc, buf);
        }
    }
}

/* ==================================================================
 *  Group 5: execve
 *
 *  Compile a program that uses execve() directly (not system()) to
 *  run /bin/echo.  This bypasses the shell and validates that
 *  compiled binaries can invoke execve correctly.
 * ================================================================== */
static void test_execve(void)
{
    printf("[execve]\n");

    /* 1. Write execve_test.c — uses execve to run /bin/echo */
    {
        int rc = write_file("/tmp/execve_test.c",
                            "#include <stdio.h>\n"
                            "#include <stdlib.h>\n"
                            "#include <string.h>\n"
                            "#include <unistd.h>\n"
                            "#include <sys/wait.h>\n"
                            "\n"
                            "int main(void) {\n"
                            "    pid_t pid = fork();\n"
                            "    if (pid < 0) return 1;\n"
                            "    if (pid == 0) {\n"
                            "        char *argv[] = { \"/bin/echo\", \"execve-ok\", NULL };\n"
                            "        char *envp[] = { NULL };\n"
                            "        execve(\"/bin/echo\", argv, envp);\n"
                            "        _exit(127);\n"
                            "    }\n"
                            "    int status;\n"
                            "    waitpid(pid, &status, 0);\n"
                            "    return (WIFEXITED(status) && WEXITSTATUS(status) == 0) ? 0 : 1;\n"
                            "}\n");
        if (rc == 0) {
            PASS("write execve_test.c");
        } else {
            FAIL("write execve_test.c", "(errno=%d)", errno);
        }
    }

    /* 2. Compile */
    {
        int rc = run("gcc -o /tmp/execve_test /tmp/execve_test.c 2>&1");
        if (rc == 0) {
            PASS("gcc compile execve_test.c");
        } else {
            FAIL("gcc compile execve_test.c", "(rc=%d)", rc);
        }
    }

    /* 3. Run and check output contains "execve-ok" */
    {
        char buf[256];
        int rc = capture("/tmp/execve_test 2>&1", buf, sizeof(buf));
        if (rc == 0 && strcmp(buf, "execve-ok") == 0) {
            PASS("run execve_test");
        } else {
            FAIL("run execve_test", "(rc=%d output='%s')", rc, buf);
        }
    }
}

/* ==================================================================
 *  Group 6: mmap
 *
 *  Compile a program that uses mmap(MAP_ANONYMOUS | MAP_PRIVATE) to
 *  allocate memory, write to it, read it back, and munmap.
 * ================================================================== */
static void test_mmap(void)
{
    printf("[mmap]\n");

    /* 1. Write mmap_test.c */
    {
        int rc = write_file("/tmp/mmap_test.c",
                            "#include <stdio.h>\n"
                            "#include <string.h>\n"
                            "#include <sys/mman.h>\n"
                            "#include <unistd.h>\n"
                            "\n"
                            "int main(void) {\n"
                            "    long page_size = sysconf(_SC_PAGESIZE);\n"
                            "    if (page_size <= 0) page_size = 4096;\n"
                            "\n"
                            "    /* MAP_ANONYMOUS | MAP_PRIVATE */\n"
                            "    void *p = mmap(NULL, page_size, PROT_READ | PROT_WRITE,\n"
                            "                   MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);\n"
                            "    if (p == MAP_FAILED) return 1;\n"
                            "\n"
                            "    /* Write and read back */\n"
                            "    memcpy(p, \"mmap-works\", 10);\n"
                            "    if (memcmp(p, \"mmap-works\", 10) != 0) { munmap(p, page_size); return 1; }\n"
                            "\n"
                            "    /* mprotect: make read-only, write should segfault — just verify mprotect succeeds */\n"
                            "    if (mprotect(p, page_size, PROT_READ) != 0) { munmap(p, page_size); return 1; }\n"
                            "\n"
                            "    /* munmap */\n"
                            "    if (munmap(p, page_size) != 0) return 1;\n"
                            "\n"
                            "    printf(\"mmap-ok\\n\");\n"
                            "    return 0;\n"
                            "}\n");
        if (rc == 0) {
            PASS("write mmap_test.c");
        } else {
            FAIL("write mmap_test.c", "(errno=%d)", errno);
        }
    }

    /* 2. Compile */
    {
        int rc = run("gcc -o /tmp/mmap_test /tmp/mmap_test.c 2>&1");
        if (rc == 0) {
            PASS("gcc compile mmap_test.c");
        } else {
            FAIL("gcc compile mmap_test.c", "(rc=%d)", rc);
        }
    }

    /* 3. Run */
    {
        char buf[256];
        int rc = capture("/tmp/mmap_test 2>&1", buf, sizeof(buf));
        if (rc == 0 && strcmp(buf, "mmap-ok") == 0) {
            PASS("run mmap_test");
        } else {
            FAIL("run mmap_test", "(rc=%d output='%s')", rc, buf);
        }
    }
}

/* ==================================================================
 *  Group 7: File locking (flock + fcntl)
 *
 *  Compile a program that:
 *  - Uses flock(LOCK_EX) for exclusive locking
 *  - Uses fcntl(F_SETLK) for POSIX record locking
 *  - Verifies lock conflict detection via forked child
 * ================================================================== */
static void test_file_locking(void)
{
    printf("[file locking]\n");

    /* 1. Write flock_test.c */
    {
        int rc = write_file("/tmp/flock_test.c",
                            "#include <stdio.h>\n"
                            "#include <stdlib.h>\n"
                            "#include <string.h>\n"
                            "#include <unistd.h>\n"
                            "#include <fcntl.h>\n"
                            "#include <sys/file.h>\n"
                            "#include <sys/wait.h>\n"
                            "\n"
                            "int main(void) {\n"
                            "    int fd = open(\"/tmp/flock_file\", O_CREAT | O_RDWR | O_TRUNC, 0644);\n"
                            "    if (fd < 0) return 1;\n"
                            "\n"
                            "    /* --- flock: exclusive lock --- */\n"
                            "    if (flock(fd, LOCK_EX) != 0) { close(fd); return 1; }\n"
                            "\n"
                            "    /* Fork child that tries LOCK_NB|LOCK_EX — must fail with EWOULDBLOCK */\n"
                            "    pid_t pid = fork();\n"
                            "    if (pid < 0) { close(fd); return 1; }\n"
                            "    if (pid == 0) {\n"
                            "        int cfd = open(\"/tmp/flock_file\", O_RDWR);\n"
                            "        if (cfd < 0) _exit(1);\n"
                            "        int ret = flock(cfd, LOCK_EX | LOCK_NB);\n"
                            "        close(cfd);\n"
                            "        _exit(ret == 0 ? 0 : 2); /* exit 0 = lock acquired (bad), 2 = EWOULDBLOCK (good) */\n"
                            "    }\n"
                            "    int status;\n"
                            "    waitpid(pid, &status, 0);\n"
                            "    int flock_conflict_ok = (WIFEXITED(status) && WEXITSTATUS(status) == 2);\n"
                            "\n"
                            "    /* Unlock */\n"
                            "    flock(fd, LOCK_UN);\n"
                            "\n"
                            "    /* --- fcntl: POSIX record lock (F_SETLK / F_GETLK) --- */\n"
                            "    struct flock fl;\n"
                            "    memset(&fl, 0, sizeof(fl));\n"
                            "    fl.l_type = F_WRLCK;\n"
                            "    fl.l_whence = SEEK_SET;\n"
                            "    fl.l_start = 0;\n"
                            "    fl.l_len = 32;\n"
                            "    if (fcntl(fd, F_SETLK, &fl) != 0) { close(fd); return 1; }\n"
                            "\n"
                            "    /* Verify lock is held: F_GETLK should overwrite l_type to F_UNLCK if no conflict,\n"
                            "     * or stay as F_WRLCK if we query a conflicting range. */\n"
                            "    struct flock fl2;\n"
                            "    memset(&fl2, 0, sizeof(fl2));\n"
                            "    fl2.l_type = F_WRLCK;\n"
                            "    fl2.l_whence = SEEK_SET;\n"
                            "    fl2.l_start = 0;\n"
                            "    fl2.l_len = 32;\n"
                            "    /* A conflicting F_GETLK from the same process should report our own lock */\n"
                            "    fcntl(fd, F_GETLK, &fl2);\n"
                            "    /* (POSIX: F_GETLK from the locking process itself may or may not report\n"
                            "     *  the lock — so we just verify fcntl didn't fail.) */\n"
                            "\n"
                            "    /* Unlock POSIX lock */\n"
                            "    fl.l_type = F_UNLCK;\n"
                            "    if (fcntl(fd, F_SETLK, &fl) != 0) { close(fd); return 1; }\n"
                            "\n"
                            "    close(fd);\n"
                            "    printf(\"%s\\n\", flock_conflict_ok ? \"flock-ok\" : \"flock-noconflict\");\n"
                            "    return flock_conflict_ok ? 0 : 1;\n"
                            "}\n");
        if (rc == 0) {
            PASS("write flock_test.c");
        } else {
            FAIL("write flock_test.c", "(errno=%d)", errno);
        }
    }

    /* 2. Compile */
    {
        int rc = run("gcc -o /tmp/flock_test /tmp/flock_test.c 2>&1");
        if (rc == 0) {
            PASS("gcc compile flock_test.c");
        } else {
            FAIL("gcc compile flock_test.c", "(rc=%d)", rc);
        }
    }

    /* 3. Run */
    {
        char buf[256];
        int rc = capture("/tmp/flock_test 2>&1", buf, sizeof(buf));
        if (rc == 0 && strcmp(buf, "flock-ok") == 0) {
            PASS("run flock_test");
        } else {
            FAIL("run flock_test", "(rc=%d output='%s')", rc, buf);
        }
    }
}

/* ==================================================================
 *  Group 8: Preprocessor & linking flags
 * ================================================================== */
static void test_flags(void)
{
    printf("[preprocessor & linking]\n");

    /* 1. -D macro */
    {
        write_file("/tmp/macro_test.c",
                   "#include <stdio.h>\n"
                   "int main(void) {\n"
                   "    printf(\"%s\\n\", MSG);\n"
                   "    return 0;\n"
                   "}\n");
        char buf[256];
        int rc = capture(
            "gcc -DMSG=\\\"defined-macro\\\" -o /tmp/macro_test /tmp/macro_test.c "
            "2>&1 && /tmp/macro_test 2>&1",
            buf, sizeof(buf));
        if (rc == 0 && strcmp(buf, "defined-macro") == 0) {
            PASS("gcc -D macro");
        } else {
            FAIL("gcc -D macro", "(rc=%d output='%s')", rc, buf);
        }
    }

    /* 2. -lm math linking */
    {
        write_file("/tmp/math_test.c",
                   "#include <stdio.h>\n"
                   "#include <math.h>\n"
                   "int main(void) {\n"
                   "    volatile double x = 2.0;\n"
                   "    double v = sqrt(x);\n"
                   "    printf(\"%.4f\\n\", v);\n"
                   "    return 0;\n"
                   "}\n");
        char buf[256];
        int rc = capture(
            "gcc -o /tmp/math_test /tmp/math_test.c -lm 2>&1 && "
            "/tmp/math_test 2>&1",
            buf, sizeof(buf));
        if (rc == 0 && strcmp(buf, "1.4142") == 0) {
            PASS("gcc -lm math linking");
        } else {
            FAIL("gcc -lm math linking", "(rc=%d output='%s')", rc, buf);
        }
    }
}

/* ==================================================================
 *  Main
 * ================================================================== */
int main(void)
{
    printf("=== GCC compilation test ===\n");

    test_tool_availability();
    test_hello_c();
    test_multi_file();
    test_fork_pipe_waitpid();
    test_execve();
    test_mmap();
    test_file_locking();
    test_flags();

    printf("=== total: %d passed, %d failed ===\n", pass, fail);

    if (fail > 0)
        return 1;
    printf("GCC TEST PASSED\n");
    return 0;
}
