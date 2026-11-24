// Regression guard for the StarryOS ELF loader's script-loading paths.
//
// `load_user_app` / `ElfLoader` load the executable from an already-resolved
// `Location` (the caller resolves once, mirroring Linux's `do_open_execat`)
// and resolve script interpreters internally. Two such interpreter paths are
// exercised here by execve()-ing scripts *directly* — no shell in between, so
// the kernel loader, not busybox, is what handles them:
//
//   - /tmp/loader-shebang     (no .sh suffix) -> kernel `#!` shebang branch:
//       not an ELF, starts with "#!", so the loader resolves the interpreter
//       (/bin/sh) via open_exec and loads it as the new image.
//   - /tmp/loader-dotsh.sh    (.sh suffix)    -> kernel `.sh` redirect branch:
//       the loader rewrites argv to "/bin/sh <path>" before any ELF load.
//
// Each child must exec the script and exit 0; only then is the final marker
// printed. A loader regression makes a child fail to exec (non-zero / signal),
// which prints a `FAIL:` line (caught by fail_regex) instead of the marker.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int write_script(const char *path, const char *body) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    if (fd < 0) {
        perror("open");
        return -1;
    }
    size_t len = strlen(body);
    if (write(fd, body, len) != (ssize_t)len) {
        perror("write");
        close(fd);
        return -1;
    }
    if (close(fd) < 0) {
        perror("close");
        return -1;
    }
    // Re-assert the exec bit in case the active umask cleared it at open().
    if (chmod(path, 0755) < 0) {
        perror("chmod");
        return -1;
    }
    return 0;
}

// fork() + execve(path) directly; returns 0 iff the child exec'd and exited 0.
static int run_exec(const char *path) {
    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return -1;
    }
    if (pid == 0) {
        char *argv[] = {(char *)path, NULL};
        char *envp[] = {NULL};
        execve(path, argv, envp);
        // execve only returns on failure.
        perror("execve");
        _exit(127);
    }
    int status = 0;
    if (waitpid(pid, &status, 0) < 0) {
        perror("waitpid");
        return -1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "child for %s did not exit cleanly (status=0x%x)\n", path,
                status);
        return -1;
    }
    return 0;
}

// fork() + execve(path) directly; returns 0 iff execve rejects the script
// with the expected errno instead of entering an unbounded interpreter loop.
static int run_expect_errno(const char *path, int expected_errno) {
    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return -1;
    }
    if (pid == 0) {
        char *argv[] = {(char *)path, NULL};
        char *envp[] = {NULL};
        execve(path, argv, envp);
        if (errno != expected_errno) {
            fprintf(stderr, "execve(%s) returned errno=%d, expected %d\n", path, errno,
                    expected_errno);
            _exit(1);
        }
        _exit(0);
    }
    int status = 0;
    if (waitpid(pid, &status, 0) < 0) {
        perror("waitpid");
        return -1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "child for %s did not report expected errno (status=0x%x)\n", path,
                status);
        return -1;
    }
    return 0;
}

int main(void) {
    // No .sh suffix -> exercises the kernel `#!` shebang branch.
    const char *shebang = "/tmp/loader-shebang";
    // .sh suffix -> exercises the kernel `.sh` redirect branch.
    const char *dotsh = "/tmp/loader-dotsh.sh";
    const char *chain0 = "/tmp/loader-shebang-chain-0";
    const char *chain1 = "/tmp/loader-shebang-chain-1";
    const char *chain2 = "/tmp/loader-shebang-chain-2";
    const char *chain3 = "/tmp/loader-shebang-chain-3";
    const char *chain4 = "/tmp/loader-shebang-chain-4";
    const char *too_deep0 = "/tmp/loader-shebang-too-deep-0";
    const char *too_deep1 = "/tmp/loader-shebang-too-deep-1";
    const char *too_deep2 = "/tmp/loader-shebang-too-deep-2";
    const char *too_deep3 = "/tmp/loader-shebang-too-deep-3";
    const char *too_deep4 = "/tmp/loader-shebang-too-deep-4";
    const char *too_deep5 = "/tmp/loader-shebang-too-deep-5";
    const char *self_loop = "/tmp/loader-shebang-self-loop";
    const char *loop_a = "/tmp/loader-shebang-loop-a";
    const char *loop_b = "/tmp/loader-shebang-loop-b";

    if (write_script(shebang, "#!/bin/sh\necho SHEBANG_RAN\n") != 0) {
        printf("FAIL: write shebang script\n");
        return 1;
    }
    if (write_script(dotsh, "#!/bin/sh\necho DOTSH_RAN\n") != 0) {
        printf("FAIL: write .sh script\n");
        return 1;
    }
    // Five script-to-interpreter rewrites are valid; the last script reaches
    // /bin/sh at recursion depth five.
    if (write_script(chain0, "#!/tmp/loader-shebang-chain-1\n") != 0 ||
        write_script(chain1, "#!/tmp/loader-shebang-chain-2\n") != 0 ||
        write_script(chain2, "#!/tmp/loader-shebang-chain-3\n") != 0 ||
        write_script(chain3, "#!/tmp/loader-shebang-chain-4\n") != 0 ||
        write_script(chain4, "#!/bin/sh\nexit 0\n") != 0) {
        printf("FAIL: write bounded shebang chain\n");
        return 1;
    }
    // A sixth script rewrite must fail before the final /bin/sh load.
    if (write_script(too_deep0, "#!/tmp/loader-shebang-too-deep-1\n") != 0 ||
        write_script(too_deep1, "#!/tmp/loader-shebang-too-deep-2\n") != 0 ||
        write_script(too_deep2, "#!/tmp/loader-shebang-too-deep-3\n") != 0 ||
        write_script(too_deep3, "#!/tmp/loader-shebang-too-deep-4\n") != 0 ||
        write_script(too_deep4, "#!/tmp/loader-shebang-too-deep-5\n") != 0 ||
        write_script(too_deep5, "#!/bin/sh\nexit 0\n") != 0) {
        printf("FAIL: write too-deep shebang chain\n");
        return 1;
    }
    if (write_script(self_loop, "#!/tmp/loader-shebang-self-loop\n") != 0 ||
        write_script(loop_a, "#!/tmp/loader-shebang-loop-b\n") != 0 ||
        write_script(loop_b, "#!/tmp/loader-shebang-loop-a\n") != 0) {
        printf("FAIL: write cyclic shebang scripts\n");
        return 1;
    }

    if (run_exec(shebang) != 0) {
        printf("FAIL: exec shebang script\n");
        return 1;
    }
    if (run_exec(dotsh) != 0) {
        printf("FAIL: exec .sh script\n");
        return 1;
    }
    if (run_exec(chain0) != 0) {
        printf("FAIL: exec bounded shebang chain\n");
        return 1;
    }
    if (run_expect_errno(too_deep0, ELOOP) != 0) {
        printf("FAIL: too-deep shebang chain did not return ELOOP\n");
        return 1;
    }
    if (run_expect_errno(self_loop, ELOOP) != 0) {
        printf("FAIL: self-referential shebang did not return ELOOP\n");
        return 1;
    }
    if (run_expect_errno(loop_a, ELOOP) != 0) {
        printf("FAIL: cyclic shebang did not return ELOOP\n");
        return 1;
    }

    printf("ELF_LOADER_SHEBANG_OK\n");
    return 0;
}
