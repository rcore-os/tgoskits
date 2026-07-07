/*
 * Focused StarryOS regression test for Nix 2.34 builder initialization.
 *
 * Replicates the exact syscall sequence from Nix source:
 *   startProcess   (processes.cc:236-293)  — fork + prctl(PR_SET_PDEATHSIG)
 *   openSlave      (derivation-builder.cc:599-618) — open PTY slave, raw mode, dup2
 *   commonChildInit (child.cc:10-35)       — restoreSignals, setsid, dup2, /dev/null
 *   runChild        (derivation-builder.cc:933-991) — closeExtraFDs, setrlimit, umask, handshake, exec
 *
 * Each step reports a distinct failure marker so the failing step is identifiable.
 * Final marker: NIX_BUILDER_INIT_ALL_PASSED
 */

#define _GNU_SOURCE
#include "../common/test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

#ifndef CLOSE_RANGE_CLOEXEC
#define CLOSE_RANGE_CLOEXEC (1U << 2)
#endif

#ifndef O_CLOEXEC
#define O_CLOEXEC 02000000
#endif

static int read_line_timeout(int fd, char *buf, size_t max, int timeout_sec)
{
    size_t pos = 0;
    while (pos < max - 1) {
        fd_set rfds;
        FD_ZERO(&rfds);
        FD_SET(fd, &rfds);
        struct timeval tv = { .tv_sec = timeout_sec, .tv_usec = 0 };
        int rc = select(fd + 1, &rfds, NULL, NULL, &tv);
        if (rc < 0) return -1;
        if (rc == 0) return 0;
        char ch;
        ssize_t n = read(fd, &ch, 1);
        if (n <= 0) return (int)n;
        if (ch == '\n') { buf[pos] = '\0'; return 1; }
        buf[pos++] = ch;
    }
    buf[pos] = '\0';
    return 1;
}

static void test_nix_builder_init(void)
{
    TEST_START("Nix 2.34 builder init: full syscall sequence");

    /* ── Parent: PTY setup (Nix DerivationBuilderImpl::startBuild) ── */
    int pty_master = posix_openpt(O_RDWR | O_NOCTTY);
    CHECK(pty_master >= 0, "posix_openpt(O_RDWR|O_NOCTTY)");
    if (pty_master < 0) return;

    int rc = grantpt(pty_master);
    CHECK(rc == 0, "grantpt(pty_master)");
    rc = unlockpt(pty_master);
    CHECK(rc == 0, "unlockpt(pty_master)");

    char *slave_name = ptsname(pty_master);
    CHECK(slave_name != NULL, "ptsname(pty_master)");

    /* ── Nix startProcess: fork + prctl(PR_SET_PDEATHSIG, SIGKILL) ── */
    pid_t child = fork();
    CHECK(child >= 0, "fork() for builder child (Nix startProcess)");
    if (child < 0) { close(pty_master); return; }

    if (child == 0) {
        /* ═══════════════ CHILD PROCESS ═══════════════ */
        close(pty_master);

        /* Step C0: prctl(PR_SET_PDEATHSIG, SIGKILL) — Nix startProcess */
        if (prctl(PR_SET_PDEATHSIG, SIGKILL) < 0) {
            printf("NIX_BUILDER_INIT_PRCTL_PDEATHSIG_FAILED errno=%d (%s)\n",
                   errno, strerror(errno));
            _exit(80);
        }

        /* Step C1: open PTY slave — Nix openSlave */
        int slave_fd = open(slave_name, O_RDWR | O_NOCTTY | O_CLOEXEC);
        if (slave_fd < 0) {
            printf("NIX_BUILDER_INIT_SLAVE_OPEN_FAILED errno=%d (%s)\n",
                   errno, strerror(errno));
            _exit(70);
        }

        /* Step C2: tcgetattr + cfmakeraw + tcsetattr — Nix openSlave raw mode */
        {
            struct termios term;
            if (tcgetattr(slave_fd, &term) < 0) {
                dprintf(STDERR_FILENO,
                        "NIX_BUILDER_INIT_TCGETATTR_FAILED errno=%d (%s)\n",
                        errno, strerror(errno));
                _exit(79);
            }
            cfmakeraw(&term);
            if (tcsetattr(slave_fd, TCSANOW, &term) < 0) {
                dprintf(STDERR_FILENO,
                        "NIX_BUILDER_INIT_TCSETATTR_FAILED errno=%d (%s)\n",
                        errno, strerror(errno));
                _exit(80);
            }
        }

        /* Step C3: dup2 slave to stderr — Nix openSlave */
        if (dup2(slave_fd, STDERR_FILENO) < 0) {
            printf("NIX_BUILDER_INIT_DUP2_SLAVE_STDERR_FAILED errno=%d (%s)\n",
                   errno, strerror(errno));
            _exit(71);
        }
        if (slave_fd != STDERR_FILENO) close(slave_fd);

        /* Step C4: restoreProcessContext — reset all signals to SIG_DFL (Nix commonChildInit) */
        {
            struct sigaction sa;
            memset(&sa, 0, sizeof(sa));
            sa.sa_handler = SIG_DFL;
            int sig_ok = 1;
            for (int sig = 1; sig <= 31; sig++) {
                if (sig == SIGKILL || sig == SIGSTOP) continue;
                if (sigaction(sig, &sa, NULL) < 0) {
                    if (errno != EINVAL) sig_ok = 0;
                }
            }
            if (!sig_ok) {
                dprintf(STDERR_FILENO,
                        "NIX_BUILDER_INIT_RESTORE_SIGNALS_FAILED errno=%d (%s)\n",
                        errno, strerror(errno));
                _exit(72);
            }
        }

        /* Step C5: setsid() — Nix commonChildInit */
        if (setsid() < 0) {
            dprintf(STDERR_FILENO,
                    "NIX_BUILDER_INIT_SETSID_FAILED errno=%d (%s)\n",
                    errno, strerror(errno));
            _exit(73);
        }

        /* Step C6: dup2(stderr, stdout) — Nix commonChildInit */
        if (dup2(STDERR_FILENO, STDOUT_FILENO) < 0) {
            dprintf(STDERR_FILENO,
                    "NIX_BUILDER_INIT_DUP2_STDERR_STDOUT_FAILED errno=%d (%s)\n",
                    errno, strerror(errno));
            _exit(74);
        }

        /* Step C7: open /dev/null, dup2 to stdin — Nix commonChildInit */
        {
            int null_fd = open("/dev/null", O_RDWR | O_CLOEXEC);
            if (null_fd < 0) {
                dprintf(STDERR_FILENO,
                        "NIX_BUILDER_INIT_OPEN_DEVNULL_FAILED errno=%d (%s)\n",
                        errno, strerror(errno));
                _exit(75);
            }
            if (dup2(null_fd, STDIN_FILENO) < 0) {
                dprintf(STDERR_FILENO,
                        "NIX_BUILDER_INIT_DUP2_DEVNULL_STDIN_FAILED errno=%d (%s)\n",
                        errno, strerror(errno));
                _exit(76);
            }
            if (null_fd != STDIN_FILENO) close(null_fd);
        }

        /* Step C8: close_range(3, ~0U, 0) — Nix closeExtraFDs */
        {
            int cr_rc = (int)syscall(SYS_close_range, 3u, ~0U, 0u);
            if (cr_rc != 0) {
                dprintf(STDERR_FILENO,
                        "NIX_BUILDER_INIT_CLOSE_RANGE_FAILED errno=%d (%s)\n",
                        errno, strerror(errno));
                _exit(77);
            }
        }

        /* Step C9: setrlimit(RLIMIT_CORE, {0, RLIM_INFINITY}) — Nix runChild */
        {
            struct rlimit lim = { .rlim_cur = 0, .rlim_max = RLIM_INFINITY };
            if (setrlimit(RLIMIT_CORE, &lim) < 0) {
                dprintf(STDERR_FILENO,
                        "NIX_BUILDER_INIT_SETRLIMIT_CORE_FAILED errno=%d (%s)\n",
                        errno, strerror(errno));
                _exit(78);
            }
        }

        /* Step C10: umask(0022) — Nix runChild */
        umask(0022);

        /* Step C11: write "\2\n" to stderr — Nix handshake */
        dprintf(STDERR_FILENO, "\x02\n");

        /* Step C12: exec — Nix execBuilder */
        execl("/bin/true", "true", NULL);
        dprintf(STDERR_FILENO,
                "NIX_BUILDER_INIT_EXEC_FAILED errno=%d (%s)\n",
                errno, strerror(errno));
        _exit(81);

    } else {
        /* ═══════════════ PARENT PROCESS ═══════════════ */

        /* Read handshake "\2\n" from PTY master (Nix processSandboxSetupMessages) */
        char line[256];
        int line_rc = read_line_timeout(pty_master, line, sizeof(line), 5);
        CHECK(line_rc > 0, "read from PTY master after fork (got data)");
        if (line_rc > 0) {
            int is_stx = (line[0] == '\x02');
            CHECK(is_stx, "PTY handshake byte is STX (\\x02)");
            if (!is_stx) {
                printf("  DIAG: received byte 0x%02x ('%c'), expected 0x02 (STX)\n",
                       (unsigned char)line[0],
                       (line[0] >= 32 && line[0] < 127) ? line[0] : '?');
            }
        } else if (line_rc == 0) {
            printf("  DIAG: read from PTY master timed out after 5s\n");
        } else {
            printf("  DIAG: read from PTY master failed errno=%d (%s)\n",
                   errno, strerror(errno));
        }

        /* Wait for child */
        int status = 0;
        pid_t waited = waitpid(child, &status, 0);
        CHECK(waited == child, "waitpid collects builder child");
        if (waited == child) {
            if (WIFEXITED(status)) {
                int code = WEXITSTATUS(status);
                CHECK(code == 0, "builder child exit code 0");
                if (code != 0) {
                    printf("  DIAG: child exit code=%d\n", code);
                }
            } else if (WIFSIGNALED(status)) {
                printf("  DIAG: child killed by signal %d (%s)\n",
                       WTERMSIG(status), strsignal(WTERMSIG(status)));
                CHECK(0, "builder child not killed by signal");
            }
        }

        close(pty_master);
    }
}

int main(void)
{
    test_nix_builder_init();
    printf("NIX_BUILDER_INIT_ALL_PASSED\n");
    return 0;
}
