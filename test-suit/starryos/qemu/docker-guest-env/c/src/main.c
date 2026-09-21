/*
 * docker-guest-env-probe
 *
 * Phase 1 gate of the StarryOS docker bring-up plan
 * (docs/design/docker-startup.md): kernel-level container ABI checks that the
 * Debian userland smoke script cannot express precisely.
 *
 * Sections:
 *   pseudofs    /proc /sys /dev /dev/shm /tmp superblock magics via statfs
 *   devpts      container-style newinstance devpts mount under /tmp
 *   pty         /dev/ptmx allocation plus master/slave byte round-trip
 *   ns          namespace links, unshare + setns round-trips, child PID ns
 *   scm-rights  AF_UNIX SCM_RIGHTS fd passing across fork
 *   cgroup2     cgroup2 mount at /sys/fs/cgroup plus controllers
 *
 * Superblock magics are the values this kernel reports (see
 * os/StarryOS/kernel/src/pseudofs), not generic Linux constants: devfs reports
 * the tmpfs magic and cgroup2 reports 0x63677270. TIOCPKT is intentionally an
 * OBSERVE-only check: the tty layer does not implement packet mode yet, and
 * this probe must stay green until that lands as a later plan increment.
 */
#define _GNU_SOURCE

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

#define DGE_PROC_MAGIC 0x9fa0UL
#define DGE_SYSFS_MAGIC 0x62656572UL
#define DGE_TMPFS_MAGIC 0x01021994UL
#define DGE_DEVTMPFS_MAGIC 0x6c647367UL
#define DGE_DEVPTS_MAGIC 0x1cd1UL
#define DGE_CGROUP2_MAGIC 0x63677270UL

#ifndef TIOCGPTN
#define TIOCGPTN 0x80045430
#endif
#ifndef TIOCSPTLCK
#define TIOCSPTLCK 0x40045431
#endif
#ifndef TIOCPKT
#define TIOCPKT 0x5420
#endif

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

static void observe(const char *msg)
{
    printf("  OBSERVE: %s\n", msg);
}

static void section(const char *name)
{
    printf("== DGE %s\n", name);
}

static int expect_fs_magic(const char *path, unsigned long magic0,
                           unsigned long magic1, const char *msg)
{
    struct statfs st;
    if (statfs(path, &st) != 0) {
        fail(msg);
        return 0;
    }
    unsigned long f_type = (unsigned long)st.f_type;
    if (f_type == magic0 || (magic1 != 0 && f_type == magic1)) {
        pass(msg);
        return 1;
    }
    printf("  FAIL: %s (f_type=0x%lx)\n", msg, f_type);
    failures++;
    return 0;
}

static int expect_read_eq(int fd, const char *want, size_t len,
                          const char *msg)
{
    char buf[32];
    size_t got = 0;
    while (got < len) {
        ssize_t n = read(fd, buf + got, sizeof(buf) - got < len - got
                                                 ? sizeof(buf) - got
                                                 : len - got);
        if (n <= 0) {
            if (n == 0) {
                errno = EIO;
            }
            fail(msg);
            return 0;
        }
        got += (size_t)n;
    }
    if (memcmp(buf, want, len) != 0) {
        errno = EPROTO;
        fail(msg);
        return 0;
    }
    pass(msg);
    return 1;
}

static int expect_write_all(int fd, const char *data, size_t len,
                            const char *msg)
{
    size_t sent = 0;
    while (sent < len) {
        ssize_t n = write(fd, data + sent, len - sent);
        if (n <= 0) {
            if (n == 0) {
                errno = EIO;
            }
            fail(msg);
            return 0;
        }
        sent += (size_t)n;
    }
    pass(msg);
    return 1;
}

/* Namespace identity follows the Linux `ns(5)` idiom: open the ns file (this
 * yields a dedicated NsFd) and fstat it; the fd's st_ino is the namespace id.
 * A plain path stat reports the static procfs node inode instead, and the NsFd
 * is not readable. */
static int ns_identity(const char *type, ino_t *inode_out)
{
    char path[64];
    int written = snprintf(path, sizeof(path), "/proc/self/ns/%s", type);
    if (written < 0 || (size_t)written >= sizeof(path)) {
        return -1;
    }
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        printf("  DIAG: open /proc/self/ns/%s errno=%d (%s)\n", type, errno,
               strerror(errno));
        return -1;
    }
    struct stat st;
    int ok = fstat(fd, &st);
    if (ok != 0) {
        printf("  DIAG: fstat /proc/self/ns/%s errno=%d (%s)\n", type, errno,
               strerror(errno));
    }
    close(fd);
    if (ok != 0) {
        return -1;
    }
    *inode_out = st.st_ino;
    return 0;
}

static void check_pseudofs(void)
{
    section("pseudofs");

    expect_fs_magic("/proc", DGE_PROC_MAGIC, 0, "/proc is procfs");
    expect_fs_magic("/sys", DGE_SYSFS_MAGIC, 0, "/sys is sysfs");
    expect_fs_magic("/dev", DGE_TMPFS_MAGIC, DGE_DEVTMPFS_MAGIC,
                    "/dev is the kernel devfs");
    expect_fs_magic("/dev/shm", DGE_TMPFS_MAGIC, 0, "/dev/shm is tmpfs");
    expect_fs_magic("/tmp", DGE_TMPFS_MAGIC, 0, "/tmp is tmpfs");

    int fd = open("/proc/self/status", O_RDONLY);
    if (fd < 0) {
        fail("open /proc/self/status");
    } else {
        char buf[64];
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n > 0) {
            pass("read /proc/self/status");
        } else {
            fail("read /proc/self/status");
        }
        close(fd);
    }

    char shm_path[] = "/dev/shm/dge-probe-XXXXXX";
    int shm_fd = mkstemp(shm_path);
    if (shm_fd < 0) {
        fail("create file under /dev/shm");
    } else {
        const char payload[] = "shm-payload";
        int ok = expect_write_all(shm_fd, payload, sizeof(payload) - 1,
                                  "write /dev/shm file") &&
                 (lseek(shm_fd, 0, SEEK_SET) == 0) &&
                 expect_read_eq(shm_fd, payload, sizeof(payload) - 1,
                                "read back /dev/shm file");
        close(shm_fd);
        unlink(shm_path);
        if (ok) {
            pass("tmpfs storage behind /dev/shm");
        }
    }
}

static void check_devpts(void)
{
    section("devpts");

    static const char mountpoint[] = "/tmp/dge-devpts";
    if (mkdir(mountpoint, 0755) != 0 && errno != EEXIST) {
        fail("create devpts newinstance mountpoint");
        return;
    }
    if (mount("none", mountpoint, "devpts", 0,
              "newinstance,mode=0620,gid=5,ptmxmode=0666") != 0) {
        fail("mount devpts newinstance");
        return;
    }

    struct statfs st;
    if (statfs(mountpoint, &st) != 0 ||
        (unsigned long)st.f_type != DGE_DEVPTS_MAGIC) {
        unsigned long f_type = statfs(mountpoint, &st) == 0
                                   ? (unsigned long)st.f_type
                                   : 0;
        printf("  FAIL: devpts newinstance reports devpts magic "
               "(f_type=0x%lx)\n",
               f_type);
        failures++;
    } else {
        pass("devpts newinstance reports devpts magic");
    }

    char ptmx_path[64];
    snprintf(ptmx_path, sizeof(ptmx_path), "%s/ptmx", mountpoint);
    DIR *dir = opendir(mountpoint);
    if (dir == NULL) {
        fail("readdir devpts newinstance");
    } else {
        int has_ptmx = 0;
        struct dirent *entry;
        while ((entry = readdir(dir)) != NULL) {
            if (strcmp(entry->d_name, "ptmx") == 0) {
                has_ptmx = 1;
            }
        }
        closedir(dir);
        if (has_ptmx) {
            pass("devpts newinstance exposes its ptmx node");
        } else {
            fail("devpts newinstance exposes its ptmx node");
        }
    }

    /* A fresh instance must start PTY numbering at zero. */
    int master = open(ptmx_path, O_RDWR | O_NOCTTY);
    if (master < 0) {
        fail("open newinstance ptmx");
    } else {
        unsigned int number = ~0U;
        if (ioctl(master, TIOCGPTN, &number) != 0) {
            fail("newinstance TIOCGPTN");
        } else if (number != 0) {
            printf("  FAIL: fresh devpts instance starts numbering at zero "
                   "(got %u)\n",
                   number);
            failures++;
        } else {
            pass("fresh devpts instance starts numbering at zero");
        }
        close(master);
    }

    umount2(mountpoint, MNT_DETACH);
    rmdir(mountpoint);
}

static void check_pty(void)
{
    section("pty");

    int master = open("/dev/ptmx", O_RDWR | O_NOCTTY);
    if (master < 0) {
        fail("open /dev/ptmx");
        return;
    }

    unsigned int number = 0;
    if (ioctl(master, TIOCGPTN, &number) != 0) {
        fail("TIOCGPTN on /dev/ptmx");
        close(master);
        return;
    }
    pass("TIOCGPTN on /dev/ptmx");

    /* grantpt is a no-op on musl; unlockpt maps to TIOCSPTLCK. */
    if (unlockpt(master) != 0) {
        fail("unlockpt on /dev/ptmx");
        close(master);
        return;
    }
    pass("unlockpt on /dev/ptmx");

    char slave_path[64];
    snprintf(slave_path, sizeof(slave_path), "/dev/pts/%u", number);
    int slave = open(slave_path, O_RDWR | O_NOCTTY);
    if (slave < 0) {
        fail("open allocated slave pty");
        close(master);
        return;
    }
    pass("open allocated slave pty");

    /* Raw mode keeps byte counts deterministic: no canonical line buffering,
     * no ONLCR newline translation on the master side. */
    struct termios tio;
    if (tcgetattr(slave, &tio) != 0) {
        fail("tcgetattr on slave pty");
        close(slave);
        close(master);
        return;
    }
    cfmakeraw(&tio);
    if (tcsetattr(slave, TCSANOW, &tio) != 0) {
        fail("tcsetattr raw mode on slave pty");
        close(slave);
        close(master);
        return;
    }

    expect_write_all(master, "dge-pty-a", 9, "master writes to slave");
    expect_read_eq(slave, "dge-pty-a", 9, "slave reads master bytes");
    expect_write_all(slave, "dge-pty-b", 9, "slave writes to master");
    expect_read_eq(master, "dge-pty-b", 9, "master reads slave bytes");

    int unlocked = 0;
    errno = 0;
    if (ioctl(master, TIOCSPTLCK, &unlocked) != 0) {
        observe("TIOCSPTLCK rejected on /dev/ptmx");
    }
    int packets = 1;
    errno = 0;
    if (ioctl(master, TIOCPKT, &packets) != 0) {
        observe("TIOCPKT unsupported: tty packet mode is a later plan "
                "increment");
    } else {
        observe("TIOCPKT accepted: revisit byte expectations if packet mode "
                "is now active");
    }

    close(slave);
    close(master);
}

static void check_namespaces(void)
{
    static const char *ns_types[] = {
        "mnt", "pid", "net", "uts", "ipc", "user", "cgroup",
    };
    section("ns");

    for (size_t i = 0; i < sizeof(ns_types) / sizeof(ns_types[0]); i++) {
        ino_t inode = 0;
        char msg[96];
        snprintf(msg, sizeof(msg), "fstat /proc/self/ns/%s", ns_types[i]);
        if (ns_identity(ns_types[i], &inode) != 0 || inode == 0) {
            fail(msg);
            continue;
        }
        printf("  PASS: %s (ns inode %lu)\n", msg, (unsigned long)inode);
    }

    /* Mount namespace: unshare, observe a new ns inode, setns back. */
    ino_t before = 0;
    ino_t after = 0;
    if (ns_identity("mnt", &before) != 0) {
        fail("fstat /proc/self/ns/mnt before unshare");
        return;
    }
    int mnt_fd = open("/proc/self/ns/mnt", O_RDONLY);
    if (mnt_fd < 0) {
        fail("open /proc/self/ns/mnt before unshare");
        return;
    }
    if (unshare(CLONE_NEWNS) != 0) {
        fail("unshare CLONE_NEWNS");
        close(mnt_fd);
        return;
    }
    if (ns_identity("mnt", &after) == 0 && after != before) {
        pass("unshare CLONE_NEWNS enters a new mount namespace");
    } else {
        errno = EPROTO;
        fail("unshare CLONE_NEWNS enters a new mount namespace");
    }
    if (setns(mnt_fd, CLONE_NEWNS) != 0) {
        fail("setns back to the parent mount namespace");
    } else {
        ino_t restored = 0;
        if (ns_identity("mnt", &restored) == 0 && restored == before) {
            pass("setns restores the parent mount namespace");
        } else {
            errno = EPROTO;
            fail("setns restores the parent mount namespace");
        }
    }
    close(mnt_fd);

    /* Network namespace: same round-trip. */
    if (ns_identity("net", &before) != 0) {
        fail("fstat /proc/self/ns/net before unshare");
        return;
    }
    int net_fd = open("/proc/self/ns/net", O_RDONLY);
    if (net_fd < 0) {
        fail("open /proc/self/ns/net before unshare");
        return;
    }
    if (unshare(CLONE_NEWNET) != 0) {
        fail("unshare CLONE_NEWNET");
        close(net_fd);
        return;
    }
    if (ns_identity("net", &after) == 0 && after != before) {
        pass("unshare CLONE_NEWNET enters a new network namespace");
    } else {
        errno = EPROTO;
        fail("unshare CLONE_NEWNET enters a new network namespace");
    }
    if (setns(net_fd, CLONE_NEWNET) != 0) {
        fail("setns back to the parent network namespace");
    } else {
        ino_t restored = 0;
        if (ns_identity("net", &restored) == 0 && restored == before) {
            pass("setns restores the parent network namespace");
        } else {
            errno = EPROTO;
            fail("setns restores the parent network namespace");
        }
    }
    close(net_fd);

    /* PID namespace: on Linux the unsharing caller keeps its own pid number;
     * only the next fork enters the new namespace, as its pid 1. The inner
     * child mounts a namespace-local procfs, mirrors what a container runtime
     * does, and reports its namespace inode over the pipe. */
    ino_t parent_pid_ns = 0;
    if (ns_identity("pid", &parent_pid_ns) != 0) {
        fail("fstat /proc/self/ns/pid before child unshare");
        return;
    }

    int report[2];
    if (pipe(report) != 0) {
        fail("create pid-ns report pipe");
        return;
    }
    pid_t child = fork();
    if (child == 0) {
        close(report[0]);
        if (unshare(CLONE_NEWPID | CLONE_NEWNS) != 0) {
            _exit(10);
        }
        pid_t inner = fork();
        if (inner == 0) {
            if (getpid() != 1) {
                _exit(11);
            }
            if (mount("proc", "/proc", "proc", 0, NULL) != 0) {
                _exit(12);
            }
            ino_t identity = 0;
            if (ns_identity("pid", &identity) != 0) {
                _exit(13);
            }
            if (write(report[1], &identity, sizeof(identity)) !=
                (ssize_t)sizeof(identity)) {
                _exit(14);
            }
            _exit(0);
        }
        if (inner <= 0) {
            _exit(15);
        }
        int inner_status = 0;
        if (waitpid(inner, &inner_status, 0) != inner ||
            !WIFEXITED(inner_status) || WEXITSTATUS(inner_status) != 0) {
            _exit(16);
        }
        _exit(0);
    }

    close(report[1]);
    ino_t child_pid_ns = 0;
    size_t received = 0;
    while (received < sizeof(child_pid_ns)) {
        ssize_t n = read(report[0], (char *)&child_pid_ns + received,
                         sizeof(child_pid_ns) - received);
        if (n <= 0) {
            break;
        }
        received += (size_t)n;
    }
    close(report[0]);

    int status = 0;
    int waited = waitpid(child, &status, 0);
    if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("  FAIL: pid-ns child exit (waited=%d status=%d)\n", waited,
               status);
        failures++;
    } else if (received != sizeof(child_pid_ns) ||
               child_pid_ns == parent_pid_ns) {
        errno = EPROTO;
        fail("child pid namespace differs from the parent");
    } else {
        pass("child pid namespace differs from the parent");
    }
}

static void check_scm_rights(void)
{
    section("scm-rights");

    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        fail("socketpair AF_UNIX SOCK_STREAM");
        return;
    }
    int p[2];
    if (pipe(p) != 0) {
        fail("create scm-rights pipe");
        close(sv[0]);
        close(sv[1]);
        return;
    }

    static const char marker = 'Z';
    pid_t child = fork();
    if (child == 0) {
        close(sv[0]);
        if (write(p[1], &marker, 1) != 1) {
            _exit(20);
        }
        char payload = 'A';
        struct iovec iov = { .iov_base = &payload, .iov_len = 1 };
        char cbuf[CMSG_SPACE(sizeof(int))];
        memset(cbuf, 0, sizeof(cbuf));
        struct msghdr mh;
        memset(&mh, 0, sizeof(mh));
        mh.msg_iov = &iov;
        mh.msg_iovlen = 1;
        mh.msg_control = cbuf;
        mh.msg_controllen = sizeof(cbuf);
        struct cmsghdr *cmh = CMSG_FIRSTHDR(&mh);
        cmh->cmsg_level = SOL_SOCKET;
        cmh->cmsg_type = SCM_RIGHTS;
        cmh->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(cmh), &p[0], sizeof(int));
        /* Attach p[0] while the sender still holds it open; dropping the
         * sender's reference afterwards does not affect the receiver's copy. */
        if (sendmsg(sv[1], &mh, 0) != 1) {
            _exit(21);
        }
        close(p[0]);
        close(p[1]);
        close(sv[1]);
        _exit(0);
    }

    /* The parent keeps its own pipe read end open until the fd has been
     * received: closing it early races the sender's write/sendmsg and turns
     * the pipe into a writer-less EPIPE trap. The marker is then read through
     * the *received* fd only, proving a real cross-process fd installation. */
    close(sv[1]);

    char rx = 0;
    struct iovec riov = { .iov_base = &rx, .iov_len = 1 };
    char rcbuf[CMSG_SPACE(sizeof(int))];
    memset(rcbuf, 0, sizeof(rcbuf));
    struct msghdr rmh;
    memset(&rmh, 0, sizeof(rmh));
    rmh.msg_iov = &riov;
    rmh.msg_iovlen = 1;
    rmh.msg_control = rcbuf;
    rmh.msg_controllen = sizeof(rcbuf);

    ssize_t r = recvmsg(sv[0], &rmh, 0);
    if (r != 1 || rx != 'A') {
        errno = r < 0 ? errno : EPROTO;
        fail("recvmsg returns the payload byte");
    } else {
        pass("recvmsg returns the payload byte");
    }

    int got_fd = -1;
    struct cmsghdr *rcmh = CMSG_FIRSTHDR(&rmh);
    if (rcmh == NULL || rcmh->cmsg_level != SOL_SOCKET ||
        rcmh->cmsg_type != SCM_RIGHTS) {
        errno = EPROTO;
        fail("received SCM_RIGHTS cmsg");
    } else {
        memcpy(&got_fd, CMSG_DATA(rcmh), sizeof(int));
        if (got_fd >= 0) {
            pass("received SCM_RIGHTS cmsg");
        } else {
            errno = EPROTO;
            fail("received SCM_RIGHTS cmsg");
        }
    }

    close(p[0]);

    if (got_fd >= 0) {
        expect_read_eq(got_fd, &marker, 1,
                       "marker byte crosses the passed fd");
        close(got_fd);
    }

    int status = 0;
    int waited = waitpid(child, &status, 0);
    if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("  FAIL: scm-rights sender exit (waited=%d status=%d)\n",
               waited, status);
        failures++;
    }

    close(p[1]);
    close(sv[0]);
}

static void check_cgroup2(void)
{
    section("cgroup2");

    struct statfs st;
    int mounted_here = 0;
    if (statfs("/sys/fs/cgroup", &st) == 0 &&
        (unsigned long)st.f_type == DGE_CGROUP2_MAGIC) {
        observe("cgroup2 already mounted at /sys/fs/cgroup by the smoke "
                "script");
    } else if (mount("none", "/sys/fs/cgroup", "cgroup2", 0, NULL) != 0) {
        fail("mount cgroup2 at /sys/fs/cgroup");
        return;
    } else {
        mounted_here = 1;
        pass("mount cgroup2 at /sys/fs/cgroup");
    }

    char buf[512];
    int fd = open("/sys/fs/cgroup/cgroup.controllers", O_RDONLY);
    if (fd < 0) {
        fail("open cgroup.controllers");
    } else {
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        if (n > 0) {
            buf[n] = '\0';
            pass("read cgroup.controllers");
            if (strstr(buf, "pids") != NULL) {
                pass("cgroup.controllers advertises the pids controller");
            } else {
                errno = EPROTO;
                fail("cgroup.controllers advertises the pids controller");
            }
        } else {
            fail("read cgroup.controllers");
        }
        close(fd);
    }

    if (mounted_here) {
        umount("/sys/fs/cgroup");
    }
}

int main(void)
{
    check_pseudofs();
    check_devpts();
    check_pty();
    check_namespaces();
    check_scm_rights();
    check_cgroup2();

    if (failures != 0) {
        printf("DOCKER_GUEST_ENV_PROBE_FAILED: failed=%d\n", failures);
        return 1;
    }
    printf("DOCKER_GUEST_ENV_PROBE_PASSED\n");
    return 0;
}
