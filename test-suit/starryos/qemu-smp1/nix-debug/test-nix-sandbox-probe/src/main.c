#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/select.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* musl may not define CLONE_NEWCGROUP */
#ifndef CLONE_NEWCGROUP
#define CLONE_NEWCGROUP 0x02000000
#endif

/* pivot_root may need explicit syscall wrapper on musl */
#ifndef SYS_pivot_root
#include <asm/unistd.h>
#endif
static int do_pivot_root(const char *new_root, const char *put_old) {
    return syscall(SYS_pivot_root, new_root, put_old);
}

/* ------------------------------------------------------------------ */
/* helpers                                                            */
/* ------------------------------------------------------------------ */

static int ptm_fd = -1;
static int pts_fd = -1;

static void die(const char *msg) {
    perror(msg);
    exit(1);
}

static void probe_report(const char *step, int ok) {
    dprintf(STDERR_FILENO, "%s:%s\n", step, ok ? "OK" : "FAIL");
    if (!ok) {
        /* write errno detail on the probe line itself */
        dprintf(STDERR_FILENO, "%s:errno=%d=%s\n", step, errno, strerror(errno));
    }
}

/* clone wrapper — mmap stack, call clone() with CLONE_PARENT */
static pid_t clone_child(int (*fn)(void *), void *arg, int flags) {
    const size_t stack_size = 256 * 1024;
    void *stack = mmap(NULL, stack_size, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    if (stack == MAP_FAILED)
        die("mmap stack");
    pid_t pid = clone(fn, (char *)stack + stack_size, flags, arg);
    if (pid < 0) {
        perror("clone");
        munmap(stack, stack_size);
    }
    return pid;
}

/* ---------------------------------------------------------------- */
/* builder child function — reproduces nix enterChroot() order      */
/* ---------------------------------------------------------------- */

static int builder_fn(void *raw) {
    char tmpdir[256];
    (void)raw;

    /* ABSOLUTE FIRST THING: prove this process ran */
    int fd = open("/tmp/builder-ran", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd >= 0) {
        dprintf(fd, "builder_pid=%d ppid=%d\n", getpid(), getppid());
        close(fd);
    }

    /* --- Phase 0: commonChildInit ------------------------------------ */
    if (setsid() < 0)
        probe_report("S01_setsid", 0);
    else
        probe_report("S01_setsid", 1);

    /* dup2 stderr -> stdout (nix does this so build output goes to PTY) */
    if (dup2(STDERR_FILENO, STDOUT_FILENO) < 0)
        probe_report("S02_dup2_err_to_out", 0);
    else
        probe_report("S02_dup2_err_to_out", 1);

    /* open /dev/null, dup2 to stdin */
    int nullfd = open("/dev/null", O_RDWR);
    if (nullfd < 0)
        probe_report("S03_open_dev_null", 0);
    else
        probe_report("S03_open_dev_null", 1);

    if (nullfd >= 0) {
        if (dup2(nullfd, STDIN_FILENO) < 0)
            probe_report("S04_dup2_null_to_stdin", 0);
        else
            probe_report("S04_dup2_null_to_stdin", 1);
        close(nullfd);
    } else {
        probe_report("S04_dup2_null_to_stdin", 0);
    }

    /* --- Phase 1: enterChroot() -------------------------------------- */

    /* 1.3 Make all mounts private */
    if (mount(0, "/", 0, MS_PRIVATE | MS_REC, 0) < 0)
        probe_report("S05_mount_private_rec", 0);
    else
        probe_report("S05_mount_private_rec", 1);

    /* Create a temp directory as simulated chroot-root */
    snprintf(tmpdir, sizeof(tmpdir), "/tmp/sp-%d", getpid());
    if (mkdir(tmpdir, 0755) < 0)
        probe_report("S06_mkdir_tmp_root", 0);
    else
        probe_report("S06_mkdir_tmp_root", 1);

    /* Bind-mount it onto itself (prerequisite for pivot_root) */
    if (mount(tmpdir, tmpdir, 0, MS_BIND, 0) < 0)
        probe_report("S07_bind_chroot", 0);
    else
        probe_report("S07_bind_chroot", 1);

    /* Create /proc inside the chroot and mount procfs */
    char procdir[512];
    snprintf(procdir, sizeof(procdir), "%s/proc", tmpdir);
    mkdir(procdir, 0555);
    if (mount("none", procdir, "proc", 0, 0) < 0)
        probe_report("S08_mount_proc", 0);
    else
        probe_report("S08_mount_proc", 1);

    /* 1.7 unshare(CLONE_NEWNS) — second unshare */
    if (unshare(CLONE_NEWNS) < 0)
        probe_report("S09_unshare_NEWNS_again", 0);
    else
        probe_report("S09_unshare_NEWNS_again", 1);

    /* 1.7 unshare(CLONE_NEWCGROUP) — NOT IMPLEMENTED on StarryOS */
    if (unshare(CLONE_NEWCGROUP) < 0)
        probe_report("S10_unshare_NEWCGROUP", 0);
    else
        probe_report("S10_unshare_NEWCGROUP", 1);

    /* 1.7 chdir + pivot_root + chroot */
    if (chdir(tmpdir) < 0)
        probe_report("S11_chdir_chroot", 0);
    else
        probe_report("S11_chdir_chroot", 1);

    mkdir("real-root", 0500);
    if (do_pivot_root(".", "./real-root") < 0)
        probe_report("S12_pivot_root", 0);
    else
        probe_report("S12_pivot_root", 1);

    if (chroot(".") < 0)
        probe_report("S13_chroot", 0);
    else
        probe_report("S13_chroot", 1);

    if (umount2("real-root", MNT_DETACH) < 0)
        probe_report("S14_umount_old_root", 0);
    else
        probe_report("S14_umount_old_root", 1);

    rmdir("real-root");
    probe_report("S15_rmdir_old_root", 1);

    /* 2.1 NO_NEW_PRIVS */
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0)
        probe_report("S16_prctl_no_new_privs", 0);
    else
        probe_report("S16_prctl_no_new_privs", 1);

    /* --- Success signal (nix writes "\2\n") ------------------------- */
    dprintf(STDERR_FILENO, "\n\\2\\nREADY\n");
    write(STDERR_FILENO, "\2\n", 2);

    /* cleanup and exit */
    _exit(0);
    return 0;
}

/* ---------------------------------------------------------------- */
/* helper process — calls clone(CLONE_PARENT|...) → builder          */
/* ---------------------------------------------------------------- */

static void helper_process(void) {
    int flags = CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWIPC | CLONE_NEWUTS |
                CLONE_PARENT | SIGCHLD;
    pid_t child = clone_child(builder_fn, NULL, flags);
    if (child < 0) {
        dprintf(STDERR_FILENO, "CLONE_FAILED:errno=%d=%s\n", errno, strerror(errno));
        _exit(1);
    }
    dprintf(STDERR_FILENO, "CLONE_OK:child_pid=%d\n", child);
    _exit(0);
}

/* ---------------------------------------------------------------- */
/* main: open PTY → fork helper → read PTY master                    */
/* ---------------------------------------------------------------- */

int main(void) {
    /* 1. Open PTY master */
    ptm_fd = posix_openpt(O_RDWR | O_NOCTTY);
    if (ptm_fd < 0)
        die("posix_openpt");
    if (grantpt(ptm_fd) < 0)
        die("grantpt");
    if (unlockpt(ptm_fd) < 0)
        die("unlockpt");

    /* 2. Open PTY slave in main process (to pass to helper+builder) */
    const char *slave_name = ptsname(ptm_fd);
    if (!slave_name)
        die("ptsname");
    pts_fd = open(slave_name, O_RDWR | O_NOCTTY);
    if (pts_fd < 0)
        die("open pts");

    /* 3. Write PTY fd to helper via pipe */
    int pfd[2];
    if (pipe(pfd) < 0)
        die("pipe");

    /* 4. Fork helper */
    pid_t helper = fork();
    if (helper < 0)
        die("fork");

    if (helper == 0) {
        /* CHILD = helper */
        close(pfd[0]); /* close read end */
        close(ptm_fd);    /* helper doesn't need master */

        /* Use dup2 to set stderr→pts_fd (matching nix pattern) */
        if (dup2(pts_fd, STDERR_FILENO) < 0)
            die("dup2 pts to stderr");
        close(pts_fd);

        helper_process();
        _exit(0);
    }

    /* PARENT = daemon */
    close(pfd[1]); /* close write end */
    close(pts_fd);
    pts_fd = -1;

    /* 5. Read from PTY master — use timer-based polling, NOT helper_done */
    printf("NIX_SANDBOX_PROBE_BEGIN\n");
    char buf[65536];
    int total = 0;
    for (int t = 0; t < 10; t++) {
        fd_set rfds;
        struct timeval tv = { 1, 0 };
        FD_ZERO(&rfds);
        FD_SET(ptm_fd, &rfds);
        int r = select(ptm_fd + 1, &rfds, NULL, NULL, &tv);
        if (r < 0 && errno != EINTR)
            break;
        if (r > 0) {
            ssize_t n = read(ptm_fd, buf + total, sizeof(buf) - total - 1);
            if (n <= 0)
                break;
            total += n;
            /* Read all available without blocking */
            while (1) {
                struct timeval tv2 = { 0, 50000 };
                FD_ZERO(&rfds);
                FD_SET(ptm_fd, &rfds);
                if (select(ptm_fd + 1, &rfds, NULL, NULL, &tv2) <= 0)
                    break;
                ssize_t m = read(ptm_fd, buf + total, sizeof(buf) - total - 1);
                if (m <= 0)
                    break;
                total += m;
            }
            /* Stop early if we have the final marker */
            if (strstr(buf, "\\\\2\\\\nREADY") || strstr(buf, "S16_prctl"))
                break;
        }
    }
    if (total > 0) {
        buf[total] = '\0';
        printf("NIX_SANDBOX_PROBE_OUTPUT_BEGIN\n%s\nNIX_SANDBOX_PROBE_OUTPUT_END\n",
               buf);
    }

    /* 6. Wait for helper */
    waitpid(helper, NULL, 0);

    printf("NIX_SANDBOX_PROBE_END\n");

    /* 7. Check results — verify all critical steps passed */
    if (strstr(buf, "S01_setsid:OK") == NULL) {
        fprintf(stderr, "FAIL: setsid\n");
        return 1;
    }
    if (strstr(buf, "S05_mount_private_rec:OK") == NULL) {
        fprintf(stderr, "FAIL: mount_private_rec\n");
        return 1;
    }
    if (strstr(buf, "S12_pivot_root:OK") == NULL) {
        fprintf(stderr, "FAIL: pivot_root\n");
        return 1;
    }
    if (strstr(buf, "S16_prctl_no_new_privs:OK") == NULL) {
        fprintf(stderr, "FAIL: prctl_no_new_privs\n");
        return 1;
    }

    printf("NIX_SANDBOX_PROBE_PASSED\n");
    return 0;
}
