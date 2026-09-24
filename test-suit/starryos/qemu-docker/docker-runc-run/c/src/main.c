/*
 * docker-runc-run-probe
 *
 * Phase 2 gate of the StarryOS docker bring-up plan
 * (docs/design/docker-startup.md): verifies the kernel semantics runc 1.1.x
 * depends on before and while running a real container.
 *
 * Checks (default subcommand, run on the host Debian rootfs):
 *   starttime       /proc/self/stat field 22 is a real ticks-since-boot
 *                   value (runc compares it as InitProcessStartTime)
 *   oom-nul         /proc/self/oom_score_adj accepts the verbatim "0\0"
 *                   write runc's nsexec issues (first NUL terminates)
 *   memfd-mode      memfd_create files carry exec permission (runc's
 *                   CVE-2019-5736 self-reexec execveat()s the memfd)
 *   pipe-fchown     fchown/fchmod succeed on pipe fds (runc adjusts its
 *                   container stdio pipes)
 *   stageb-gate     emits whether the bpf(2) cgroup-device stub is present,
 *                   so the smoke script can gate stage B (cgroups enabled)
 *
 * `fork-eagain` subcommand (runs inside the pids-limited container): forks
 * until the cgroup pids limit rejects with EAGAIN.
 *
 * Final markers: DOCKER_RUNC_RUN_PROBE_PASSED /
 * DOCKER_RUNC_RUN_PROBE_FAILED: failed=<n>.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef __NR_bpf
#error "__NR_bpf required"
#endif

/* enum bpf_cmd / bpf_attach_type values from Linux uapi/linux/bpf.h. */
/* enum bpf_cmd values (uapi/linux/bpf.h). */
#define DRR_BPF_MAP_CREATE 0
#define DRR_BPF_PROG_LOAD 5
#define DRR_BPF_PROG_ATTACH 8
#define DRR_BPF_PROG_DETACH 9
#define DRR_BPF_PROG_GET_NEXT_ID 11
#define DRR_BPF_PROG_GET_FD_BY_ID 13
#define DRR_BPF_PROG_QUERY 16
#define DRR_BPF_LINK_CREATE 28
#define DRR_BPF_OBJ_GET_INFO_BY_FD 15
#define DRR_BPF_ATTACH_CGROUP_DEVICE 6
#define DRR_BPF_PROG_TYPE_CGROUP_DEVICE 15
#define DRR_BPF_MAP_TYPE_ARRAY 2

static int failures;

static void pass(const char *msg)
{
    printf("  PASS: %s\n", msg);
}

static void fail(const char *msg)
{
    printf("  FAIL: %s (errno=%d: %s)\n", msg, errno, strerror(errno));
    failures++;
}

static void section(const char *name)
{
    printf("== DRR %s\n", name);
}

/* /proc/<pid>/stat field 22 (starttime). The comm field may contain spaces
 * and parentheses, so split at the LAST ')' and count from there. */
static int read_starttime_ticks(pid_t pid, long *ticks_out)
{
    char path[64];
    int written = snprintf(path, sizeof(path), "/proc/%d/stat", pid);
    if (written < 0 || (size_t)written >= sizeof(path)) {
        return -1;
    }

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    char buf[1024];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0) {
        return -1;
    }
    buf[n] = '\0';

    char *close_paren = strrchr(buf, ')');
    if (close_paren == NULL) {
        return -1;
    }
    /* Fields after comm: state is field 3; starttime is field 22, i.e. the
     * 20th whitespace-separated token after the closing parenthesis. */
    char *cursor = close_paren + 1;
    int field_after_comm = 3;
    while (*cursor == ' ' || *cursor == '\t') {
        cursor++;
    }
    for (int field = field_after_comm; field < 22; field++) {
        while (*cursor != ' ' && *cursor != '\t' && *cursor != '\0') {
            cursor++;
        }
        if (*cursor == '\0') {
            return -1;
        }
        while (*cursor == ' ' || *cursor == '\t') {
            cursor++;
        }
    }
    char *end = NULL;
    long value = strtol(cursor, &end, 10);
    if (end == cursor) {
        return -1;
    }
    *ticks_out = value;
    return 0;
}

static void check_starttime(void)
{
    section("starttime");

    long parent_ticks = 0;
    if (read_starttime_ticks(getpid(), &parent_ticks) != 0) {
        fail("read /proc/self/stat starttime");
        return;
    }
    if (parent_ticks > 0) {
        pass("starttime is a real ticks-since-boot value");
    } else {
        fail("starttime is a real ticks-since-boot value");
    }

    pid_t child = fork();
    if (child == 0) {
        long child_ticks = 0;
        if (read_starttime_ticks(getpid(), &child_ticks) != 0) {
            _exit(1);
        }
        /* The child was created after the parent, so its starttime must not
         * be earlier. */
        _exit(child_ticks >= parent_ticks ? 0 : 2);
    }
    int status = 0;
    int waited = waitpid(child, &status, 0);
    if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        errno = EPROTO;
        fail("child starttime is not earlier than the parent's");
    } else {
        pass("child starttime is not earlier than the parent's");
    }
}

static int open_oom_score_adj(int flags)
{
    return open("/proc/self/oom_score_adj", flags);
}

static void check_oom_nul_write(void)
{
    section("oom-nul");

    int fd = open_oom_score_adj(O_WRONLY);
    if (fd < 0) {
        fail("open oom_score_adj for the NUL write");
        return;
    }
    /* Exactly the payload runc's nsexec writes. */
    ssize_t written = write(fd, "0\0", 2);
    int saved_errno = errno;
    close(fd);
    errno = saved_errno;
    if (written != 2) {
        fail("oom_score_adj accepts the verbatim \"0\\0\" write");
        return;
    }
    pass("oom_score_adj accepts the verbatim \"0\\0\" write");

    char buf[32];
    fd = open_oom_score_adj(O_RDONLY);
    if (fd < 0) {
        fail("read back oom_score_adj");
        return;
    }
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0) {
        errno = EIO;
        fail("read back oom_score_adj");
        return;
    }
    buf[n] = '\0';
    if (strtol(buf, NULL, 10) == 0) {
        pass("oom_score_adj reports the NUL-terminated write");
    } else {
        errno = EPROTO;
        fail("oom_score_adj reports the NUL-terminated write");
    }

    /* Out-of-range with a trailing NUL must still be rejected. */
    fd = open_oom_score_adj(O_WRONLY);
    if (fd < 0) {
        fail("open oom_score_adj for the negative control");
        return;
    }
    written = write(fd, "1001\0", 5);
    saved_errno = errno;
    close(fd);
    errno = saved_errno;
    if (written == -1 && saved_errno == EINVAL) {
        pass("oom_score_adj rejects an out-of-range NUL-terminated value");
    } else {
        fail("oom_score_adj rejects an out-of-range NUL-terminated value");
    }
}

static void check_memfd_mode(void)
{
    section("memfd-mode");

    int fd = (int)syscall(SYS_memfd_create, "docker-runc-run-probe", 0);
    if (fd < 0) {
        fail("memfd_create");
        return;
    }
    struct stat st;
    if (fstat(fd, &st) != 0) {
        fail("fstat memfd");
        close(fd);
        return;
    }
    if ((st.st_mode & 0777) == 0777) {
        pass("memfd_create files are 0777 like Linux");
    } else {
        printf("  FAIL: memfd_create files are 0777 like Linux (mode=0%lo)\n",
               (unsigned long)(st.st_mode & 07777));
        failures++;
    }

    /* runc/OCI tighten the mode of the memfd it re-executes; fchmod must
     * update the underlying inode, not silently succeed on a mode-less
     * anonymous fd. */
    errno = 0;
    if (fchmod(fd, 0600) != 0) {
        fail("fchmod on a memfd fd succeeds");
        close(fd);
        return;
    }
    if (fstat(fd, &st) != 0) {
        fail("fstat memfd after fchmod");
        close(fd);
        return;
    }
    if ((st.st_mode & 0777) == 0600) {
        pass("fchmod on a memfd fd updates the inode mode");
    } else {
        printf("  FAIL: fchmod on a memfd fd updates the inode mode "
               "(mode=0%lo)\n",
               (unsigned long)(st.st_mode & 07777));
        failures++;
    }
    close(fd);
}

static void check_pipe_fchown(void)
{
    section("pipe-fchown");

    int p[2];
    if (pipe(p) != 0) {
        fail("create pipe");
        return;
    }

    /* Linux pipes carry a real inode: fchmod/fchown must persist and be
     * visible through fstat, shared by both ends. */
    struct stat r, w;
    if (fchmod(p[1], 0640) == 0 && fstat(p[0], &r) == 0 && fstat(p[1], &w) == 0 &&
        (r.st_mode & 0777) == 0640 && (w.st_mode & 0777) == 0640) {
        pass("fchmod on a pipe persists to both ends' fstat");
    } else {
        fail("fchmod on a pipe persists to both ends' fstat");
    }

    struct stat owned;
    if (fchown(p[0], 1000, 1000) == 0 && fstat(p[0], &owned) == 0 &&
        owned.st_uid == 1000 && owned.st_gid == 1000) {
        pass("fchown on a pipe persists to fstat");
    } else {
        fail("fchown on a pipe persists to fstat");
    }

    close(p[0]);
    close(p[1]);
}

/* Asserts the cgroup-device BPF capability is refused (no faked success).
 * Non-rootless runc cannot tolerate the refusal: its cgroup v2 device manager
 * fails at `bpf_prog_query(BPF_CGROUP_DEVICE)` and aborts container init, so
 * the pids/cgroups stage cannot run without a real device controller. Close
 * the gate; the smoke script then skips stage B. Writes the stage-B gate file
 * consumed by the smoke script. */
static void check_stageb_gate(void)
{
    section("stageb-gate");

    unsigned long long attr[2] = {
        0xffffffffULL,  /* target_fd: deliberately invalid */
        DRR_BPF_ATTACH_CGROUP_DEVICE,
    };
    errno = 0;
    long rc = syscall(__NR_bpf, DRR_BPF_PROG_DETACH, attr, sizeof(attr));
    int saved_errno = errno;

    if (rc == -1 && (saved_errno == EOPNOTSUPP || saved_errno == EINVAL)) {
        pass("bpf cgroup-device capability refused (no fake success)");
        printf("  OBSERVE: stage B disabled (device controller unsupported, "
               "errno=%d)\n", saved_errno);
    } else {
        printf("  FAIL: expected an explicit cgroup-device refusal, rc=%ld "
               "errno=%d\n",
               rc, saved_errno);
        failures++;
    }

    FILE *f = fopen("/tmp/drr-stageb", "w");
    if (f == NULL) {
        fail("write stage-B gate file");
        return;
    }
    /* Any value other than "1" disables stage B in the smoke script. */
    fputs("0", f);
    fclose(f);
}

/* Runs INSIDE the pids-limited container: the init (this process) is one
 * task, `resources.pids.limit = 2` allows exactly one fork, the next fork
 * must fail with EAGAIN. */
static int run_fork_eagain(void)
{
    pid_t child = fork();
    if (child < 0) {
        printf("  FAIL: first fork inside the pids container failed "
               "(errno=%d)\n",
               errno);
        return 1;
    }
    if (child == 0) {
        for (;;) {
            pause();
        }
    }

    errno = 0;
    pid_t blocked = fork();
    int fork_errno = errno;
    if (blocked >= 0) {
        /* Limit is 2 and we already have init + one child: this fork must
         * have been rejected. */
        kill(blocked, SIGKILL);
        waitpid(blocked, NULL, 0);
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        printf("  FAIL: fork succeeded despite pids.max\n");
        return 1;
    }
    if (fork_errno != EAGAIN) {
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        printf("  FAIL: fork over pids.max failed with errno=%d, want EAGAIN\n",
               fork_errno);
        return 1;
    }

    kill(child, SIGKILL);
    waitpid(child, NULL, 0);
    printf("  PASS: fork over pids.max fails with EAGAIN\n");
    printf("DOCKER_RUNC_RUN_PIDS_EAGAIN_OK\n");
    return 0;
}

/* Minimal epoll level-triggered sanity check without Go: a registered,
 * empty socketpair end must block epoll_wait(timeout = -1) instead of
 * spinning. Reports iterations per second on the serial. */
static int run_epoll_spin(void)
{
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0, sv) != 0) {
        perror("socketpair");
        return 1;
    }
    int ep = epoll_create1(EPOLL_CLOEXEC);
    if (ep < 0) {
        perror("epoll_create1");
        return 1;
    }
    struct epoll_event ev = { .events = EPOLLIN, .data.fd = sv[0] };
    if (epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev) != 0) {
        perror("epoll_ctl");
        return 1;
    }

    /* A writer pokes sv[1] once after ~500 ms; the registered end must wake
     * epoll_wait exactly then (level-triggered, no spurious spin). */
    pid_t writer = fork();
    if (writer == 0) {
        struct timespec pause = { .tv_sec = 0, .tv_nsec = 500 * 1000 * 1000 };
        nanosleep(&pause, NULL);
        char byte = 'x';
        ssize_t ignored = write(sv[1], &byte, 1);
        (void)ignored;
        _exit(0);
    }
    close(sv[1]);

    struct timespec pause = { .tv_sec = 0, .tv_nsec = 50 * 1000 * 1000 };
    long iterations = 0;
    long wakeups_with_data = 0;
    for (;;) {
        struct epoll_event out = { 0 };
        int n = epoll_wait(ep, &out, 1, -1);
        iterations++;
        if (n > 0 && (out.events & EPOLLIN) != 0) {
            char byte = 0;
            ssize_t r = read(sv[0], &byte, 1);
            if (r == 1) {
                wakeups_with_data++;
                printf("EPOLL-OK: blocked then woke with data, "
                       "iterations=%ld\n",
                       iterations);
                break;
            }
            if (r < 0 && errno == EAGAIN) {
                printf("EPOLL_SPURIOUS_READABLE at iteration %ld\n",
                       iterations);
            }
        }
        if (iterations % 50000 == 0) {
            printf("EPOLL-SPIN: iterations=%ld wakeups_with_data=%ld\n",
                   iterations, wakeups_with_data);
        }
        nanosleep(&pause, NULL);
    }
    waitpid(writer, NULL, 0);
    close(sv[0]);
    close(ep);
    return 0;
}

/* Byte-addressed bpf_attr builder: the stub's attribute layouts place u32
 * fields and aligned u64 pointers at fixed offsets, so tests fill an
 * all-zero buffer field by field. */
static unsigned char drr_attr[64];

static void drr_set_u32(int offset, uint32_t value)
{
    memcpy(drr_attr + offset, &value, sizeof(value));
}

static void drr_set_u64(int offset, uint64_t value)
{
    memcpy(drr_attr + offset, &value, sizeof(value));
}

static long drr_bpf(uint32_t cmd)
{
    return syscall(__NR_bpf, cmd, drr_attr, sizeof(drr_attr));
}

/* Cgroup-device BPF capability regression: the kernel does not enforce
 * device access control, so the whole device-controller family must be
 * refused with EOPNOTSUPP instead of faking a policy that never takes
 * effect. Ordinary commands, fds and ids keep falling through to the real
 * handlers. */
static int run_bpf_lifecycle(void)
{
    section("bpf-lifecycle");

    /* Ordinary map fd: OBJ_GET_INFO_BY_FD carries no prog_type, so it must
     * reach the real dispatcher (baseline EINVAL) and leave the buffer
     * untouched. */
    memset(drr_attr, 0, sizeof(drr_attr));
    drr_set_u32(0, DRR_BPF_MAP_TYPE_ARRAY);
    drr_set_u32(4, 4);   /* key_size */
    drr_set_u32(8, 8);   /* value_size */
    drr_set_u32(12, 4);  /* max_entries */
    errno = 0;
    int map_fd = (int)drr_bpf(DRR_BPF_MAP_CREATE);
    if (map_fd < 0) {
        fail("create array map");
        return 1;
    }
    uint32_t map_info[2] = { 0xAAAAAAAA, 0xAAAAAAAA };
    memset(drr_attr, 0, sizeof(drr_attr));
    drr_set_u32(0, (uint32_t)map_fd);
    drr_set_u32(4, sizeof(map_info));
    drr_set_u64(8, (uint64_t)(uintptr_t)map_info);
    errno = 0;
    if (drr_bpf(DRR_BPF_OBJ_GET_INFO_BY_FD) != -1 || errno != EINVAL ||
        map_info[0] != 0xAAAAAAAA || map_info[1] != 0xAAAAAAAA) {
        fail("OBJ_GET_INFO on an ordinary map fd falls through (EINVAL, no write)");
        close(map_fd);
        return 1;
    }
    close(map_fd);

    /* Ordinary ids: no synthetic program-id space exists, so enumeration and
     * lookup fall through with the generic unsupported EINVAL. */
    memset(drr_attr, 0, sizeof(drr_attr));
    errno = 0;
    if (drr_bpf(DRR_BPF_PROG_GET_NEXT_ID) != -1 || errno != EINVAL) {
        fail("GET_NEXT_ID falls through with EINVAL");
        return 1;
    }
    memset(drr_attr, 0, sizeof(drr_attr));
    drr_set_u32(0, 12345);
    errno = 0;
    if (drr_bpf(DRR_BPF_PROG_GET_FD_BY_ID) != -1 || errno != EINVAL) {
        fail("GET_FD_BY_ID falls through with EINVAL");
        return 1;
    }

    /* The device-controller family is refused with EOPNOTSUPP -- never a
     * placeholder fd, attachment record or synthetic info. The fds are
     * deliberately invalid so a fake EBADF-validation path cannot pass. */
    memset(drr_attr, 0, sizeof(drr_attr));
    drr_set_u32(0, DRR_BPF_PROG_TYPE_CGROUP_DEVICE);  /* prog_type */
    errno = 0;
    if (drr_bpf(DRR_BPF_PROG_LOAD) != -1 || errno != EOPNOTSUPP) {
        fail("PROG_LOAD cgroup-device is refused with EOPNOTSUPP");
        return 1;
    }

    memset(drr_attr, 0, sizeof(drr_attr));
    drr_set_u32(0, 0xffffffffU);                 /* target_fd: invalid */
    drr_set_u32(4, 0xffffffffU);                 /* attach_bpf_fd: invalid */
    drr_set_u32(8, DRR_BPF_ATTACH_CGROUP_DEVICE);
    errno = 0;
    if (drr_bpf(DRR_BPF_PROG_ATTACH) != -1 || errno != EOPNOTSUPP) {
        fail("PROG_ATTACH cgroup-device is refused with EOPNOTSUPP");
        return 1;
    }

    memset(drr_attr, 0, sizeof(drr_attr));
    drr_set_u32(0, 0xffffffffU);                 /* target_fd: invalid */
    drr_set_u32(8, DRR_BPF_ATTACH_CGROUP_DEVICE);
    errno = 0;
    if (drr_bpf(DRR_BPF_PROG_DETACH) != -1 || errno != EOPNOTSUPP) {
        fail("PROG_DETACH cgroup-device is refused with EOPNOTSUPP");
        return 1;
    }

    memset(drr_attr, 0, sizeof(drr_attr));
    drr_set_u32(0, 0xffffffffU);                 /* target_fd: invalid */
    drr_set_u32(4, DRR_BPF_ATTACH_CGROUP_DEVICE);
    errno = 0;
    if (drr_bpf(DRR_BPF_PROG_QUERY) != -1 || errno != EOPNOTSUPP) {
        fail("PROG_QUERY cgroup-device is refused with EOPNOTSUPP");
        return 1;
    }

    memset(drr_attr, 0, sizeof(drr_attr));
    drr_set_u32(0, 0xffffffffU);                 /* prog_fd: invalid */
    drr_set_u32(4, 0xffffffffU);                 /* target_fd: invalid */
    drr_set_u32(8, DRR_BPF_ATTACH_CGROUP_DEVICE);
    errno = 0;
    if (drr_bpf(DRR_BPF_LINK_CREATE) != -1 || errno != EOPNOTSUPP) {
        fail("LINK_CREATE cgroup-device is refused with EOPNOTSUPP");
        return 1;
    }

    return 0;
}

int main(int argc, char **argv)
{
    if (argc > 1 && strcmp(argv[1], "fork-eagain") == 0) {
        return run_fork_eagain();
    }
    if (argc > 1 && strcmp(argv[1], "bpf-lifecycle") == 0) {
        return run_bpf_lifecycle();
    }
    if (argc > 1 && strcmp(argv[1], "epoll-spin") == 0) {
        return run_epoll_spin();
    }

    check_starttime();
    check_oom_nul_write();
    check_memfd_mode();
    check_pipe_fchown();
    check_stageb_gate();

    if (failures != 0) {
        printf("DOCKER_RUNC_RUN_PROBE_FAILED: failed=%d\n", failures);
        return 1;
    }
    printf("DOCKER_RUNC_RUN_PROBE_PASSED\n");
    return 0;
}
