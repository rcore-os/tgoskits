#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <net/if.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/ioctl.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

#define CHILD_STACK_SIZE (128 * 1024)

struct sock_filter {
    unsigned short code;
    unsigned char jt;
    unsigned char jf;
    unsigned int k;
};

struct sock_fprog {
    unsigned short len;
    struct sock_filter *filter;
};

#define BPF_RET 0x06
#define BPF_K 0x00
#define SECCOMP_SET_MODE_FILTER 1
#define SECCOMP_RET_ALLOW 0x7fff0000U

struct child_context {
    int sync_read;
    int sync_write;
    char root[96];
    char executable[256];
};

static void fail(const char *stage)
{
    printf("TEST_NIXPKGS_FIRST_DIVERGENCE_FAILED stage=%s errno=%d (%s)\n",
           stage, errno, strerror(errno));
    exit(1);
}

static void child_fail(const char *stage)
{
    dprintf(STDOUT_FILENO,
            "TEST_NIXPKGS_FIRST_DIVERGENCE_FAILED stage=%s errno=%d (%s)\n",
            stage, errno, strerror(errno));
    _exit(1);
}

static void stage(const char *name)
{
    dprintf(STDOUT_FILENO, "TEST_NIXPKGS_FIRST_DIVERGENCE_STAGE %s\n", name);
}

static void timeout_handler(int signo)
{
    (void)signo;
    static const char marker[] =
        "TEST_NIXPKGS_FIRST_DIVERGENCE_FAILED stage=timeout\n";
    ssize_t ignored = write(STDOUT_FILENO, marker, sizeof(marker) - 1);
    (void)ignored;
    _exit(1);
}

static void write_all(int fd, const void *buffer, size_t length,
                      const char *failure_stage)
{
    const char *cursor = buffer;
    while (length > 0) {
        ssize_t written = write(fd, cursor, length);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            fail(failure_stage);
        }
        cursor += written;
        length -= (size_t)written;
    }
}

static void read_all(int fd, void *buffer, size_t length,
                     const char *failure_stage)
{
    char *cursor = buffer;
    while (length > 0) {
        ssize_t count = read(fd, cursor, length);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            fail(failure_stage);
        }
        if (count == 0) {
            errno = EPIPE;
            fail(failure_stage);
        }
        cursor += count;
        length -= (size_t)count;
    }
}

static void write_proc_file(pid_t pid, const char *name, const char *value)
{
    char path[128];
    int length = snprintf(path, sizeof(path), "/proc/%d/%s", pid, name);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        fail(name);
    }

    int fd = open(path, O_WRONLY);
    if (fd < 0)
        fail(name);
    write_all(fd, value, strlen(value), name);
    if (close(fd) < 0)
        fail(name);
}

static void open_namespace(pid_t pid, const char *name)
{
    char path[128];
    int length = snprintf(path, sizeof(path), "/proc/%d/ns/%s", pid, name);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        fail(name);
    }

    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        fail(name);
    if (close(fd) < 0)
        fail(name);
}

static void make_directory(const char *path)
{
    if (mkdir(path, 0755) < 0 && errno != EEXIST)
        child_fail(path);
}

static void make_file(const char *path)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_CLOEXEC, 0755);
    if (fd < 0)
        child_fail(path);
    if (close(fd) < 0)
        child_fail(path);
}

static void require_character_device(const char *path, const char *failure_stage)
{
    struct stat status;
    if (stat(path, &status) < 0)
        child_fail(failure_stage);
    if (!S_ISCHR(status.st_mode)) {
        errno = ENOTTY;
        child_fail(failure_stage);
    }
}

static void bind_runtime_directory(const char *root, const char *source,
                                   const char *failure_stage, int required)
{
    dprintf(STDOUT_FILENO,
            "TEST_NIXPKGS_FIRST_DIVERGENCE_STAGE %s-enter\n",
            failure_stage);

    struct stat source_stat;
    if (stat(source, &source_stat) < 0) {
        if (!required && errno == ENOENT)
            return;
        child_fail(failure_stage);
    }
    if (!S_ISDIR(source_stat.st_mode)) {
        errno = ENOTDIR;
        child_fail(failure_stage);
    }
    dprintf(STDOUT_FILENO,
            "TEST_NIXPKGS_FIRST_DIVERGENCE_STAGE %s-source-ok\n",
            failure_stage);

    char target[128];
    int length = snprintf(target, sizeof(target), "%s%s", root, source);
    if (length < 0 || (size_t)length >= sizeof(target))
        child_fail(failure_stage);
    make_directory(target);
    dprintf(STDOUT_FILENO,
            "TEST_NIXPKGS_FIRST_DIVERGENCE_STAGE %s-target-ok\n",
            failure_stage);
    if (mount(source, target, NULL, MS_BIND | MS_REC, NULL) < 0)
        child_fail(failure_stage);
    dprintf(STDOUT_FILENO,
            "TEST_NIXPKGS_FIRST_DIVERGENCE_STAGE %s-mount-ok\n",
            failure_stage);
}

static int child_main(void *opaque)
{
    struct child_context *context = opaque;
    close(context->sync_write);
    stage("child-entered");

    char token;
    ssize_t count;
    do {
        count = read(context->sync_read, &token, 1);
    } while (count < 0 && errno == EINTR);
    if (count != 1 || token != '1')
        child_fail("sync-read");
    close(context->sync_read);
    stage("sync-released");

    int socket_fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (socket_fd < 0)
        child_fail("loopback-socket");
    struct ifreq request;
    memset(&request, 0, sizeof(request));
    memcpy(request.ifr_name, "lo", sizeof("lo"));
    request.ifr_flags = IFF_UP | IFF_LOOPBACK | IFF_RUNNING;
    if (ioctl(socket_fd, SIOCSIFFLAGS, &request) < 0)
        child_fail("loopback-up");
    close(socket_fd);
    stage("loopback-up");

    if (sethostname("localhost", strlen("localhost")) < 0)
        child_fail("sethostname");
    if (setdomainname("(none)", strlen("(none)")) < 0)
        child_fail("setdomainname");
    stage("uts-configured");

    if (mount(NULL, "/", NULL, MS_PRIVATE | MS_REC, NULL) < 0)
        child_fail("mount-private-root");
    stage("root-private");

    make_directory(context->root);
    char old_root[128];
    int length = snprintf(old_root, sizeof(old_root), "%s/real-root",
                          context->root);
    if (length < 0 || (size_t)length >= sizeof(old_root))
        child_fail("old-root-path");
    make_directory(old_root);
    if (mount(context->root, context->root, NULL, MS_BIND, NULL) < 0)
        child_fail("bind-new-root");
    stage("new-root-bound");

    char builder_path[128];
    length = snprintf(builder_path, sizeof(builder_path), "%s/builder",
                      context->root);
    if (length < 0 || (size_t)length >= sizeof(builder_path))
        child_fail("builder-path");
    make_file(builder_path);
    if (mount(context->executable, builder_path, NULL, MS_BIND, NULL) < 0)
        child_fail("bind-builder");
    stage("builder-bound");

    bind_runtime_directory(context->root, "/bin", "bind-bin-runtime", 1);
    bind_runtime_directory(context->root, "/lib", "bind-lib-runtime", 1);
    bind_runtime_directory(context->root, "/lib64", "bind-lib64-runtime", 0);
    bind_runtime_directory(context->root, "/usr", "bind-usr-runtime", 0);
    stage("dynamic-runtime-bound");

    char store_path[128];
    length = snprintf(store_path, sizeof(store_path), "%s/nix", context->root);
    if (length < 0 || (size_t)length >= sizeof(store_path))
        child_fail("nix-path");
    make_directory(store_path);
    length = snprintf(store_path, sizeof(store_path), "%s/nix/store",
                      context->root);
    if (length < 0 || (size_t)length >= sizeof(store_path))
        child_fail("store-path");
    make_directory(store_path);
    if (mount(store_path, store_path, NULL, MS_BIND, NULL) < 0)
        child_fail("bind-store");
    if (mount(NULL, store_path, NULL, MS_SHARED, NULL) < 0)
        child_fail("share-store");
    stage("store-shared");

    char proc_path[128];
    length = snprintf(proc_path, sizeof(proc_path), "%s/proc", context->root);
    if (length < 0 || (size_t)length >= sizeof(proc_path))
        child_fail("proc-path");
    make_directory(proc_path);
    if (mount("none", proc_path, "proc", 0, NULL) < 0)
        child_fail("mount-proc");
    stage("proc-mounted");

    char sys_path[128];
    length = snprintf(sys_path, sizeof(sys_path), "%s/sys", context->root);
    if (length < 0 || (size_t)length >= sizeof(sys_path))
        child_fail("sys-path");
    make_directory(sys_path);
    if (mount("none", sys_path, "sysfs", 0, NULL) < 0)
        child_fail("mount-sysfs");
    stage("sysfs-mounted");

    char dev_path[128];
    length = snprintf(dev_path, sizeof(dev_path), "%s/dev", context->root);
    if (length < 0 || (size_t)length >= sizeof(dev_path))
        child_fail("dev-path");
    make_directory(dev_path);
    char shm_path[128];
    length = snprintf(shm_path, sizeof(shm_path), "%s/shm", dev_path);
    if (length < 0 || (size_t)length >= sizeof(shm_path))
        child_fail("shm-path");
    make_directory(shm_path);
    if (mount("none", shm_path, "tmpfs", 0, "mode=1777") < 0)
        child_fail("mount-dev-shm");
    stage("dev-shm-mounted");

    require_character_device("/dev/pts/ptmx", "stat-root-devpts-ptmx");
    stage("root-devpts-ptmx-stat-ok");

    char pts_path[128];
    length = snprintf(pts_path, sizeof(pts_path), "%s/pts", dev_path);
    if (length < 0 || (size_t)length >= sizeof(pts_path))
        child_fail("pts-path");
    make_directory(pts_path);
    if (mount("none", pts_path, "devpts", 0, "newinstance,mode=0620") < 0)
        child_fail("mount-devpts");
    stage("devpts-mounted");
    char ptmx_path[128];
    length = snprintf(ptmx_path, sizeof(ptmx_path), "%s/ptmx", pts_path);
    if (length < 0 || (size_t)length >= sizeof(ptmx_path))
        child_fail("ptmx-path");
    require_character_device(ptmx_path, "stat-devpts-ptmx");
    stage("devpts-ptmx-stat-ok");

    if (unshare(CLONE_NEWNS) < 0)
        child_fail("second-unshare-mount");
    stage("second-mount-namespace");

    if (chdir(context->root) < 0)
        child_fail("chdir-new-root");
    if (syscall(SYS_pivot_root, ".", "real-root") < 0)
        child_fail("pivot-root");
    stage("pivot-complete");

    if (chroot(".") < 0)
        child_fail("chroot-new-root");
    if (chdir("/") < 0)
        child_fail("chdir-root");
    if (umount2("/real-root", MNT_DETACH) < 0)
        child_fail("detach-old-root");
    if (rmdir("/real-root") < 0)
        child_fail("remove-old-root");
    stage("old-root-detached");

    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0)
        child_fail("no-new-privs");
    stage("no-new-privs");

    struct sock_filter allow = {
        .code = BPF_RET | BPF_K,
        .jt = 0,
        .jf = 0,
        .k = SECCOMP_RET_ALLOW,
    };
    struct sock_fprog filter = {
        .len = 1,
        .filter = &allow,
    };
    if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &filter) < 0)
        child_fail("seccomp-allow-filter");
    stage("seccomp-filter-installed");
    static const char handshake[] = "\x02\n";
    write_all(STDERR_FILENO, handshake, sizeof(handshake) - 1,
              "write-builder-handshake");
    stage("handshake-sent");
    stage("exec-enter");
    execl("/bin/sh", "/bin/sh", "-c", "exec /builder --sandbox-child", NULL);
    child_fail("exec-builder-shell");
    return 1;
}

int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc == 2 && strcmp(argv[1], "--sandbox-child") == 0) {
        stage("exec-complete");
        return 37;
    }

    signal(SIGALRM, timeout_handler);
    alarm(20);

    int pty_master = posix_openpt(O_RDWR | O_NOCTTY);
    if (pty_master < 0)
        fail("posix-openpt");
    if (grantpt(pty_master) < 0)
        fail("grantpt");
    if (unlockpt(pty_master) < 0)
        fail("unlockpt");

    int sync_pipe[2];
    int pid_pipe[2];
    if (pipe(sync_pipe) < 0 || pipe(pid_pipe) < 0)
        fail("pipes");

    pid_t helper = fork();
    if (helper < 0)
        fail("fork-helper");
    if (helper == 0) {
        close(sync_pipe[1]);
        close(pid_pipe[0]);

        char *slave_name = ptsname(pty_master);
        if (slave_name == NULL)
            fail("ptsname");
        int slave = open(slave_name, O_RDWR | O_NOCTTY | O_CLOEXEC);
        if (slave < 0)
            fail("open-pty-slave");
        struct termios attributes;
        if (tcgetattr(slave, &attributes) < 0)
            fail("tcgetattr-pty-slave");
        cfmakeraw(&attributes);
        if (tcsetattr(slave, TCSANOW, &attributes) < 0)
            fail("tcsetattr-pty-slave");
        if (dup2(slave, STDERR_FILENO) < 0)
            fail("dup2-pty-stderr");
        if (slave != STDERR_FILENO)
            close(slave);
        close(pty_master);

        void *stack = mmap(NULL, CHILD_STACK_SIZE, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (stack == MAP_FAILED)
            fail("child-stack");
        struct child_context context = {
            .sync_read = sync_pipe[0],
            .sync_write = sync_pipe[1],
        };
        int length = snprintf(context.root, sizeof(context.root),
                              "/tmp/nix-enter-chroot-%d", getpid());
        if (length < 0 || (size_t)length >= sizeof(context.root))
            fail("new-root-path");
        length = snprintf(context.executable, sizeof(context.executable),
                          "%s", argv[0]);
        if (length < 0 || (size_t)length >= sizeof(context.executable))
            fail("executable-path");

        int flags = CLONE_PARENT | CLONE_NEWPID | CLONE_NEWUSER | CLONE_NEWNS |
                    CLONE_NEWIPC | CLONE_NEWUTS | CLONE_NEWNET | SIGCHLD;
        pid_t child = clone(child_main, (char *)stack + CHILD_STACK_SIZE, flags,
                            &context);
        if (child < 0)
            fail("legacy-clone");
        write_all(pid_pipe[1], &child, sizeof(child), "send-child-pid");
        _exit(0);
    }

    close(sync_pipe[0]);
    close(pid_pipe[1]);

    int status;
    if (waitpid(helper, &status, 0) != helper || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        errno = ECHILD;
        fail("helper-status");
    }

    pid_t child;
    read_all(pid_pipe[0], &child, sizeof(child), "receive-child-pid");
    close(pid_pipe[0]);
    stage("parent-received-pid");

    char mapping[64];
    int length = snprintf(mapping, sizeof(mapping), "0 %u 1\n", getuid());
    if (length < 0 || (size_t)length >= sizeof(mapping)) {
        errno = EOVERFLOW;
        fail("uid-mapping");
    }
    write_proc_file(child, "uid_map", mapping);
    write_proc_file(child, "setgroups", "deny\n");
    length = snprintf(mapping, sizeof(mapping), "0 %u 1\n", getgid());
    if (length < 0 || (size_t)length >= sizeof(mapping)) {
        errno = EOVERFLOW;
        fail("gid-mapping");
    }
    write_proc_file(child, "gid_map", mapping);
    open_namespace(child, "mnt");
    open_namespace(child, "user");
    stage("parent-configured-namespaces");

    write_all(sync_pipe[1], "1", 1, "release-child");
    close(sync_pipe[1]);

    char handshake[2];
    read_all(pty_master, handshake, sizeof(handshake),
             "read-builder-handshake");
    if (handshake[0] != '\x02' || handshake[1] != '\n') {
        errno = EPROTO;
        fail("builder-handshake-content");
    }
    stage("parent-received-handshake");

    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 37) {
        errno = ECHILD;
        fail("child-status");
    }
    close(pty_master);

    alarm(0);
    puts("TEST_NIXPKGS_FIRST_DIVERGENCE_PASSED");
    return 0;
}
