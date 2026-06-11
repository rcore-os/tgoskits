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

int main(void) {
    // No .sh suffix -> exercises the kernel `#!` shebang branch.
    const char *shebang = "/tmp/loader-shebang";
    // .sh suffix -> exercises the kernel `.sh` redirect branch.
    const char *dotsh = "/tmp/loader-dotsh.sh";

    if (write_script(shebang, "#!/bin/sh\necho SHEBANG_RAN\n") != 0) {
        printf("FAIL: write shebang script\n");
        return 1;
    }
    if (write_script(dotsh, "#!/bin/sh\necho DOTSH_RAN\n") != 0) {
        printf("FAIL: write .sh script\n");
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

    printf("ELF_LOADER_SHEBANG_OK\n");
    return 0;
}
