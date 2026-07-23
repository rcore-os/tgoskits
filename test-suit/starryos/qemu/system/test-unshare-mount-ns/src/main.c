#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __NR_setns
#error "__NR_setns required from <sys/syscall.h>"
#endif

#define BASE "/tmp/nix-prereq-mnt-ns"
#define SOURCE BASE "/source"
#define TARGET BASE "/target"
#define SOURCE_MARKER SOURCE "/setns-visible"
#define TARGET_MARKER TARGET "/setns-visible"

#define FAIL(msg)                                                              \
    do {                                                                       \
        fprintf(stderr, "FAIL | %s:%d | %s: %s\n", __FILE__, __LINE__, msg,    \
                strerror(errno));                                              \
        exit(1);                                                               \
    } while (0)

#define PASS(msg)                                                              \
    do {                                                                       \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);             \
    } while (0)

static int xsetns(int fd, int nstype) {
    return (int)syscall(__NR_setns, fd, nstype);
}

static void test_unprivileged_mount_operations(void) {
    const char *missing = "/tmp/starry-unprivileged-umount-missing";
    unlink(missing);
    rmdir(missing);

    pid_t pid = fork();
    if (pid < 0)
        FAIL("fork unprivileged mount checks");
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) < 0)
            _exit(1);
        errno = 0;
        if (unshare(CLONE_NEWNS) != -1 || errno != EPERM)
            _exit(2);
        errno = 0;
        if (umount2(missing, 0) != -1 || errno != ENOENT)
            _exit(3);
        errno = 0;
        if (umount2("", 0) != -1 || errno != ENOENT)
            _exit(4);
        errno = 0;
        if (umount2("/", MNT_DETACH) != -1 || errno != EPERM)
            _exit(5);
        _exit(0);
    }

    int status = 0;
    if (waitpid(pid, &status, 0) != pid)
        FAIL("waitpid unprivileged mount checks");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        errno = EPERM;
        FAIL("unprivileged mount-operation errno priority mismatch");
    }
    PASS("unprivileged umount2 preserves path errors before EPERM");
}

static void write_all(int fd, const char *buf, size_t len, const char *what) {
    size_t done = 0;
    while (done < len) {
        ssize_t n = write(fd, buf + done, len - done);
        if (n < 0)
            FAIL(what);
        done += (size_t)n;
    }
}

static void read_one(int fd, const char *what) {
    char byte;
    ssize_t n = read(fd, &byte, 1);
    if (n != 1)
        FAIL(what);
}

static void child_exit_with_status(int fd, unsigned char status) {
    while (write(fd, &status, 1) < 0 && errno == EINTR) {}
    _exit(status);
}

static void prepare_tree(void) {
    mkdir(BASE, 0755);
    mkdir(SOURCE, 0755);
    mkdir(TARGET, 0755);

    int fd = open(SOURCE_MARKER, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0)
        FAIL("create source marker");
    write_all(fd, "mount namespace marker\n", 23, "write source marker");
    if (close(fd) < 0)
        FAIL("close source marker");

    if (access(SOURCE, F_OK) < 0)
        FAIL("source directory exists");
    if (access(TARGET, F_OK) < 0)
        FAIL("target directory exists");
    if (access(TARGET_MARKER, F_OK) == 0) {
        errno = EEXIST;
        FAIL("parent target starts without marker");
    }
    if (errno != ENOENT)
        FAIL("check parent target marker absence");
}

/* ── mountinfo record parser ──────────────────────────────────────── */

typedef struct {
    int mount_id;
    int parent_id;
    int shared_id;  /* N from shared:N tag, 0 if absent */
    int master_id;  /* N from master:N tag, 0 if absent */
} mntrec_t;

/*
 * Parse /proc/self/mountinfo for the line whose mount_point == mp.
 * Returns 0 on success, -1 if not found or parse error (errno = ENOENT).
 */
static int mountinfo_rec(const char *mp, mntrec_t *r) {
    FILE *f = fopen("/proc/self/mountinfo", "r");
    if (!f) {
        errno = ENOENT;
        return -1;
    }
    char line[2048];
    memset(r, 0, sizeof(*r));
    int found = 0;
    while (fgets(line, sizeof(line), f)) {
        char buf[2048], *toks[64];
        int n = 0;
        strncpy(buf, line, sizeof(buf) - 1);
        buf[sizeof(buf) - 1] = '\0';
        char *save;
        for (char *p = strtok_r(buf, " \t\n", &save); p && n < 64;
             p = strtok_r(NULL, " \t\n", &save))
            toks[n++] = p;
        if (n < 5)
            continue;
        if (strcmp(toks[4], mp) != 0)
            continue;
        r->mount_id  = atoi(toks[0]);
        r->parent_id = atoi(toks[1]);
        for (int i = 5; i < n; i++) {
            if (strcmp(toks[i], "-") == 0)
                break;
            if (strncmp(toks[i], "shared:", 7) == 0)
                r->shared_id = atoi(toks[i] + 7);
            else if (strncmp(toks[i], "master:", 7) == 0)
                r->master_id = atoi(toks[i] + 7);
        }
        found = 1;
        break;
    }
    fclose(f);
    return found ? 0 : -1;
}

/* ── clone(CLONE_FS) + unshare(CLONE_NEWNS) isolation test ──────────── */

#define CLONE_BASE "/tmp/nix-prereq-mnt-ns-cf"
#define CLONE_SRC CLONE_BASE "/src"
#define CLONE_DST CLONE_BASE "/dst"
#define CLONE_MARKER CLONE_SRC "/cf-marker"
#define CLONE_DONE CLONE_DST "/cf-marker"
#define CLONE_STACK_SIZE (64 * 1024)

static volatile int clone_ns_child_done;

static int clone_child_ns(void *arg) {
    (void)arg;
    if (unshare(CLONE_NEWNS) < 0) _exit(1);
    if (mount(CLONE_SRC, CLONE_DST, "none", MS_BIND, NULL) < 0) _exit(2);
    if (access(CLONE_DONE, F_OK) < 0) _exit(3);
    clone_ns_child_done = 1;
    _exit(0);
    return 0;
}

static void test_clone_fs_unshare_new_ns(void) {
    printf("\n--- clone(CLONE_FS) + child unshare(CLONE_NEWNS) ---\n");

    clone_ns_child_done = 0;

    mkdir(CLONE_BASE, 0755);
    mkdir(CLONE_SRC, 0755);
    mkdir(CLONE_DST, 0755);
    int fd = open(CLONE_MARKER, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0) FAIL("clone: create marker");
    write_all(fd, "clone ns marker\n", 16, "clone: write marker");
    close(fd);

    char *stack = mmap(NULL, CLONE_STACK_SIZE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (stack == MAP_FAILED) FAIL("clone: mmap stack");

    /*
     * clone(CLONE_FS | CLONE_VM | SIGCHLD) — parent and child share
     * Arc<Mutex<FsContext>>, child privatises before unshare(NEWNS).
     */
    pid_t pid = clone(clone_child_ns, (void *)(stack + CLONE_STACK_SIZE),
                      CLONE_FS | CLONE_VM | SIGCHLD, NULL);
    if (pid < 0) FAIL("clone: clone(CLONE_FS|CLONE_VM|SIGCHLD)");

    while (!clone_ns_child_done) usleep(10000);
    PASS("clone child unshared NEWNS + bind mount");

    /*
     * With the fix (namespace.rs force_read_decrement path):
     * child privatised FsContext before unshare_mount_namespace().
     * Parent's FsContext is unchanged → parent does NOT see the bind mount.
     */
    if (access(CLONE_DONE, F_OK) == 0) {
        fprintf(stderr, "FAIL | %s:%d | parent sees clone child mount "
                "(NEWNS on shared FsContext leaked)\n", __FILE__, __LINE__);
        exit(1);
    }
    PASS("parent does not see clone child mount (isolation ok)");

    int status;
    if (waitpid(pid, &status, 0) < 0) FAIL("clone: waitpid");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
        FAIL("clone: child non-zero exit");

    munmap(stack, CLONE_STACK_SIZE);
    rmdir(CLONE_DST);
    unlink(CLONE_MARKER);
    rmdir(CLONE_SRC);
    rmdir(CLONE_BASE);

    printf("UNSHARE_MOUNT_NS_CLONE_ISOLATION_PASSED\n");
}

/* ── Task 2.1: CLONE_NEWNS shared peer group preservation ─────────── */
/*
 * After unshare(CLONE_NEWNS), the child mount tree must preserve the
 * shared peer group membership.  Mount IDs must differ across
 * namespaces, but the shared:N (peer group) ID must be the same.
 */

#define PROP_PEER_BASE "/tmp/nix-prop-peer"
#define PROP_PEER_SRC  PROP_PEER_BASE "/src"
#define PROP_PEER_DST  PROP_PEER_BASE "/dst"
#define PROP_PEER_FROM_PARENT PROP_PEER_SRC "/from-parent"
#define PROP_PEER_FROM_CHILD  PROP_PEER_DST "/from-child"

static void test_clone_ns_shared_peer(void) {
    printf("\n--- CLONE_NEWNS shared peer group preservation ---\n");

    rmdir(PROP_PEER_DST);
    rmdir(PROP_PEER_SRC);
    rmdir(PROP_PEER_BASE);

    mkdir(PROP_PEER_BASE, 0755);
    mkdir(PROP_PEER_SRC, 0755);
    mkdir(PROP_PEER_DST, 0755);

    if (syscall(SYS_mount, "tmpfs", PROP_PEER_SRC, "tmpfs", 0, NULL) < 0)
        FAIL("prop-peer: mount tmpfs src");
    if (syscall(SYS_mount, NULL, PROP_PEER_SRC, NULL, MS_SHARED, NULL) < 0)
        FAIL("prop-peer: make src shared");
    if (syscall(SYS_mount, PROP_PEER_SRC, PROP_PEER_DST, NULL, MS_BIND, NULL) < 0)
        FAIL("prop-peer: bind src -> dst");

    mntrec_t p_src, p_dst;
    if (mountinfo_rec(PROP_PEER_SRC, &p_src) < 0)
        FAIL("prop-peer: parent src mountinfo");
    if (mountinfo_rec(PROP_PEER_DST, &p_dst) < 0)
        FAIL("prop-peer: parent dst mountinfo");
    if (p_src.shared_id == 0 || p_dst.shared_id == 0)
        FAIL("prop-peer: parent mounts lack shared:N");
    if (p_src.shared_id != p_dst.shared_id)
        FAIL("prop-peer: parent src/dst share same peer group");
    PASS("prop-peer: parent mounts in peer group");

    int parent_to_child[2];
    int child_to_parent[2];
    if (pipe(parent_to_child) < 0 || pipe(child_to_parent) < 0)
        FAIL("prop-peer: pipes");

    pid_t pid = fork();
    if (pid < 0)
        FAIL("prop-peer: fork");
    if (pid == 0) {
        close(parent_to_child[1]);
        close(child_to_parent[0]);
        if (unshare(CLONE_NEWNS) < 0)
            _exit(1);

        mntrec_t c_src, c_dst;
        if (mountinfo_rec(PROP_PEER_SRC, &c_src) < 0)
            _exit(2);
        if (mountinfo_rec(PROP_PEER_DST, &c_dst) < 0)
            _exit(3);

        /* Mount IDs must differ across namespaces. */
        if (c_src.mount_id == p_src.mount_id)
            _exit(4);
        if (c_dst.mount_id == p_dst.mount_id)
            _exit(4);

        /* Shared group IDs must be preserved. */
        if (c_src.shared_id != p_src.shared_id)
            _exit(5);
        if (c_dst.shared_id != p_dst.shared_id)
            _exit(5);
        if (c_src.shared_id != c_dst.shared_id)
            _exit(5);

        write_all(child_to_parent[1], "R", 1, "prop-peer child ready");
        read_one(parent_to_child[0], "prop-peer wait parent mount");

        mntrec_t from_parent;
        if (mountinfo_rec(PROP_PEER_FROM_PARENT, &from_parent) < 0)
            _exit(6);

        if (mkdir(PROP_PEER_FROM_CHILD, 0755) < 0 && errno != EEXIST)
            _exit(7);
        if (syscall(SYS_mount, "tmpfs", PROP_PEER_FROM_CHILD, "tmpfs", 0, NULL) < 0)
            _exit(8);
        write_all(child_to_parent[1], "C", 1, "prop-peer child mounted");
        read_one(parent_to_child[0], "prop-peer wait cleanup");
        _exit(0);
    }

    close(parent_to_child[0]);
    close(child_to_parent[1]);
    read_one(child_to_parent[0], "prop-peer wait child ready");

    PASS("prop-peer: child mount IDs differ from parent");
    PASS("prop-peer: child shared group IDs preserved");

    if (mkdir(PROP_PEER_FROM_PARENT, 0755) < 0 && errno != EEXIST)
        FAIL("prop-peer: mkdir parent event path");
    if (syscall(SYS_mount, "tmpfs", PROP_PEER_FROM_PARENT, "tmpfs", 0, NULL) < 0)
        FAIL("prop-peer: parent mount event");
    write_all(parent_to_child[1], "P", 1, "prop-peer release child");
    read_one(child_to_parent[0], "prop-peer wait child mount");

    mntrec_t from_child;
    if (mountinfo_rec(PROP_PEER_SRC "/from-child", &from_child) < 0)
        FAIL("prop-peer: child event propagated to parent namespace");
    PASS("prop-peer: mount events propagate in both namespace directions");

    if (syscall(SYS_umount2, PROP_PEER_FROM_PARENT, MNT_DETACH) < 0)
        FAIL("prop-peer: detach parent event");
    if (syscall(SYS_umount2, PROP_PEER_SRC "/from-child", MNT_DETACH) < 0)
        FAIL("prop-peer: detach child event");
    write_all(parent_to_child[1], "D", 1, "prop-peer cleanup complete");

    int status;
    if (waitpid(pid, &status, 0) < 0)
        FAIL("prop-peer: waitpid");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
        FAIL("prop-peer: child non-zero exit");

    if (syscall(SYS_umount2, PROP_PEER_SRC, MNT_DETACH) < 0)
        FAIL("prop-peer: umount2 src");
    mntrec_t detached;
    if (mountinfo_rec(PROP_PEER_SRC, &detached) == 0 ||
        mountinfo_rec(PROP_PEER_DST, &detached) < 0) {
        errno = EBUSY;
        FAIL("prop-peer: independent top-level peer state after detach");
    }
    if (syscall(SYS_umount2, PROP_PEER_DST, MNT_DETACH) < 0)
        FAIL("prop-peer: umount2 dst");
    if (mountinfo_rec(PROP_PEER_DST, &detached) == 0) {
        errno = EBUSY;
        FAIL("prop-peer: dst remains after explicit detach");
    }
    rmdir(PROP_PEER_DST);
    rmdir(PROP_PEER_SRC);
    rmdir(PROP_PEER_BASE);

    printf("UNSHARE_MOUNT_NS_SHARED_PEER_PASSED\n");
}

/* ── Task 2.2: clone + bidirectional propagation & master/slave ───── */
/*
 * After unshare(CLONE_NEWNS), verify within the child namespace:
 *  1. shared peers receive bidirectional mount propagation,
 *  2. a mount under the master propagates to the slave,
 *  3. a mount under the slave does NOT propagate back to the master.
 */

#define PROP_MS_BASE    "/tmp/nix-prop-ms"
#define PROP_MS_SRC     PROP_MS_BASE "/src"
#define PROP_MS_PEER    PROP_MS_BASE "/peer"
#define PROP_MS_SLAVE   PROP_MS_BASE "/slave"
#define PROP_MS_SUB_P2P PROP_MS_SRC "/ch-peer-sub"
#define PROP_MS_SUB_M2S PROP_MS_SRC "/ch-m2s"
#define PROP_MS_SUB_S2M PROP_MS_SLAVE "/ch-s2m"

static void test_clone_ns_master_slave(void) {
    printf("\n--- CLONE_NEWNS master/slave directionality ---\n");

    syscall(SYS_umount2, PROP_MS_SLAVE, MNT_DETACH);
    syscall(SYS_umount2, PROP_MS_PEER, MNT_DETACH);
    syscall(SYS_umount2, PROP_MS_SRC, MNT_DETACH);
    rmdir(PROP_MS_SLAVE);
    rmdir(PROP_MS_PEER);
    rmdir(PROP_MS_SRC);
    rmdir(PROP_MS_BASE);

    mkdir(PROP_MS_BASE, 0755);
    mkdir(PROP_MS_SRC, 0755);
    mkdir(PROP_MS_PEER, 0755);
    mkdir(PROP_MS_SLAVE, 0755);

    if (syscall(SYS_mount, "tmpfs", PROP_MS_SRC, "tmpfs", 0, NULL) < 0)
        FAIL("prop-ms: mount tmpfs src");
    if (syscall(SYS_mount, NULL, PROP_MS_SRC, NULL, MS_SHARED, NULL) < 0)
        FAIL("prop-ms: make src shared");
    if (syscall(SYS_mount, PROP_MS_SRC, PROP_MS_PEER, NULL, MS_BIND, NULL) < 0)
        FAIL("prop-ms: bind src -> peer");

    /* Slave: first bind (inherits shared), then promote to slave. */
    if (syscall(SYS_mount, PROP_MS_SRC, PROP_MS_SLAVE, NULL, MS_BIND, NULL) < 0)
        FAIL("prop-ms: bind src -> slave");
    if (syscall(SYS_mount, NULL, PROP_MS_SLAVE, NULL, MS_SLAVE, NULL) < 0)
        FAIL("prop-ms: make slave type");

    mntrec_t p_slv;
    if (mountinfo_rec(PROP_MS_SLAVE, &p_slv) < 0)
        FAIL("prop-ms: parent slave mountinfo");
    if (p_slv.master_id == 0)
        FAIL("prop-ms: parent slave lacks master:N tag");
    PASS("prop-ms: parent slave mountinfo ok");

    int ready_pipe[2];
    if (pipe(ready_pipe) < 0)
        FAIL("prop-ms: pipe");

    pid_t pid = fork();
    if (pid < 0)
        FAIL("prop-ms: fork");
    if (pid == 0) {
        close(ready_pipe[0]);
        if (unshare(CLONE_NEWNS) < 0)
            child_exit_with_status(ready_pipe[1], 1);

        /* ── 2.2.1  bidirectional peer propagation ── */
        if (mkdir(PROP_MS_SUB_P2P, 0755) < 0)
            child_exit_with_status(ready_pipe[1], 2);
        if (syscall(SYS_mount, "tmpfs", PROP_MS_SUB_P2P, "tmpfs", 0, NULL) < 0)
            child_exit_with_status(ready_pipe[1], 3);
        mntrec_t peer_event;
        if (mountinfo_rec(PROP_MS_PEER "/ch-peer-sub", &peer_event) < 0)
            child_exit_with_status(ready_pipe[1], 4);
        if (syscall(SYS_umount2, PROP_MS_SUB_P2P, MNT_DETACH) < 0)
            child_exit_with_status(ready_pipe[1], 5);
        rmdir(PROP_MS_SUB_P2P);

        /* ── 2.2.2  master → slave propagation ── */
        if (mkdir(PROP_MS_SUB_M2S, 0755) < 0)
            child_exit_with_status(ready_pipe[1], 6);
        if (syscall(SYS_mount, "tmpfs", PROP_MS_SUB_M2S, "tmpfs", 0, NULL) < 0)
            child_exit_with_status(ready_pipe[1], 7);
        mntrec_t slave_event;
        if (mountinfo_rec(PROP_MS_SLAVE "/ch-m2s", &slave_event) < 0)
            child_exit_with_status(ready_pipe[1], 8);
        if (syscall(SYS_umount2, PROP_MS_SUB_M2S, MNT_DETACH) < 0)
            child_exit_with_status(ready_pipe[1], 9);
        rmdir(PROP_MS_SUB_M2S);

        /* ── 2.2.3  slave → master does NOT propagate ── */
        if (mkdir(PROP_MS_SUB_S2M, 0755) < 0)
            child_exit_with_status(ready_pipe[1], 10);
        if (syscall(SYS_mount, "tmpfs", PROP_MS_SUB_S2M, "tmpfs", 0, NULL) < 0)
            child_exit_with_status(ready_pipe[1], 11);
        mntrec_t reverse_event;
        if (mountinfo_rec(PROP_MS_SRC "/ch-s2m", &reverse_event) == 0)
            child_exit_with_status(ready_pipe[1], 12);
        if (syscall(SYS_umount2, PROP_MS_SUB_S2M, MNT_DETACH) < 0)
            child_exit_with_status(ready_pipe[1], 13);
        rmdir(PROP_MS_SUB_S2M);

        child_exit_with_status(ready_pipe[1], 0);
    }

    close(ready_pipe[1]);
    unsigned char child_status;
    ssize_t n;
    do {
        n = read(ready_pipe[0], &child_status, 1);
    } while (n < 0 && errno == EINTR);
    if (n != 1)
        FAIL("prop-ms wait child");

    int status;
    if (waitpid(pid, &status, 0) < 0)
        FAIL("prop-ms: waitpid");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0 || child_status != 0) {
        errno = child_status;
        FAIL("prop-ms: child non-zero exit");
    }

    PASS("prop-ms: bidirectional peer propagation within child ns");
    PASS("prop-ms: master->slave propagation");
    PASS("prop-ms: slave-/>master (no reverse propagation)");

    syscall(SYS_umount2, PROP_MS_SLAVE, MNT_DETACH);
    syscall(SYS_umount2, PROP_MS_PEER, MNT_DETACH);
    syscall(SYS_umount2, PROP_MS_SRC, MNT_DETACH);
    rmdir(PROP_MS_SLAVE);
    rmdir(PROP_MS_PEER);
    rmdir(PROP_MS_SRC);
    rmdir(PROP_MS_BASE);

    printf("UNSHARE_MOUNT_NS_CLONE_MASTER_SLAVE_PASSED\n");
}

/* ── Task 2.3: CLONE_NEWNS transitive slave-of-peer propagation ──── */
/*
 * Regression for the propagation graph walk: when the parent namespace
 * owns `src(shared) -> slave(slave of src)`, an unshared child namespace
 * ends up with `src_c(peer of src) -> slave_c(slave of src_c)`. Adding a
 * mount under `src` must reach `slave_c` too, which is only reachable by
 * walking past `src_c` — direct iteration over `src.peers + src.slaves`
 * stops at `src_c` and the parent's `slave`, leaving the child namespace
 * `slave_c` without the new mount.
 */

#define PROP_TR_BASE   "/tmp/nix-prop-tr"
#define PROP_TR_SRC    PROP_TR_BASE "/src"
#define PROP_TR_SLAVE  PROP_TR_BASE "/slave"
#define PROP_TR_SLOT   PROP_TR_SRC "/slot"

static void test_clone_ns_transitive_slave_propagation(void) {
    printf("\n--- CLONE_NEWNS transitive slave-of-peer propagation ---\n");

    syscall(SYS_umount2, PROP_TR_SLAVE, MNT_DETACH);
    syscall(SYS_umount2, PROP_TR_SRC, MNT_DETACH);
    rmdir(PROP_TR_SLAVE);
    rmdir(PROP_TR_SRC);
    rmdir(PROP_TR_BASE);

    if (mkdir(PROP_TR_BASE, 0755) < 0)
        FAIL("prop-tr: mkdir base");
    if (mkdir(PROP_TR_SRC, 0755) < 0)
        FAIL("prop-tr: mkdir src");
    if (mkdir(PROP_TR_SLAVE, 0755) < 0)
        FAIL("prop-tr: mkdir slave");

    if (syscall(SYS_mount, "tmpfs", PROP_TR_SRC, "tmpfs", 0, NULL) < 0)
        FAIL("prop-tr: mount src tmpfs");
    if (syscall(SYS_mount, NULL, PROP_TR_SRC, NULL, MS_SHARED, NULL) < 0)
        FAIL("prop-tr: make src shared");
    if (syscall(SYS_mount, PROP_TR_SRC, PROP_TR_SLAVE, NULL, MS_BIND, NULL) < 0)
        FAIL("prop-tr: bind src -> slave");
    if (syscall(SYS_mount, NULL, PROP_TR_SLAVE, NULL, MS_SLAVE, NULL) < 0)
        FAIL("prop-tr: make slave type");

    mntrec_t p_slv;
    if (mountinfo_rec(PROP_TR_SLAVE, &p_slv) < 0)
        FAIL("prop-tr: parent slave mountinfo");
    if (p_slv.master_id == 0)
        FAIL("prop-tr: parent slave lacks master:N tag");

    int ready_pipe[2];
    int release_pipe[2];
    if (pipe(ready_pipe) < 0 || pipe(release_pipe) < 0)
        FAIL("prop-tr: pipe");

    pid_t pid = fork();
    if (pid < 0)
        FAIL("prop-tr: fork");

    if (pid == 0) {
        close(ready_pipe[0]);
        close(release_pipe[1]);

        if (unshare(CLONE_NEWNS) < 0)
            child_exit_with_status(ready_pipe[1], 1);

        write_all(ready_pipe[1], "R", 1, "prop-tr child ready");
        read_one(release_pipe[0], "prop-tr wait parent mount");

        mntrec_t src_slot;
        if (mountinfo_rec(PROP_TR_SLOT, &src_slot) < 0)
            child_exit_with_status(ready_pipe[1], 2);

        mntrec_t slave_slot;
        if (mountinfo_rec(PROP_TR_SLAVE "/slot", &slave_slot) < 0)
            child_exit_with_status(ready_pipe[1], 3);

        write_all(ready_pipe[1], "C", 1, "prop-tr child verified");
        read_one(release_pipe[0], "prop-tr cleanup");
        _exit(0);
    }

    close(ready_pipe[1]);
    close(release_pipe[0]);
    read_one(ready_pipe[0], "prop-tr wait child ready");

    if (mkdir(PROP_TR_SLOT, 0755) < 0)
        FAIL("prop-tr: mkdir slot");
    if (syscall(SYS_mount, "tmpfs", PROP_TR_SLOT, "tmpfs", 0, NULL) < 0)
        FAIL("prop-tr: mount slot");

    mntrec_t slot_info;
    if (mountinfo_rec(PROP_TR_SLOT, &slot_info) < 0)
        FAIL("prop-tr: parent slot mountinfo");

    write_all(release_pipe[1], "P", 1, "prop-tr release child");

    unsigned char child_status;
    ssize_t n;
    do {
        n = read(ready_pipe[0], &child_status, 1);
    } while (n < 0 && errno == EINTR);
    if (n != 1)
        FAIL("prop-tr: child verification lost");
    if (child_status == 2)
        FAIL("prop-tr: child did not see propagated mount on cloned shared peer");
    if (child_status == 3)
        FAIL("prop-tr: child did not see transitive propagation on cloned slave");
    if (child_status != 0) {
        errno = child_status;
        FAIL("prop-tr: child unshare or setup failed");
    }

    write_all(release_pipe[1], "D", 1, "prop-tr cleanup complete");

    int status;
    if (waitpid(pid, &status, 0) < 0)
        FAIL("prop-tr: waitpid");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
        FAIL("prop-tr: child non-zero exit");

    PASS("prop-tr: parent mount reached cloned slave via peer-of-slave graph walk");

    if (syscall(SYS_umount2, PROP_TR_SLOT, MNT_DETACH) < 0)
        FAIL("prop-tr: umount slot");
    if (syscall(SYS_umount2, PROP_TR_SLAVE, MNT_DETACH) < 0)
        FAIL("prop-tr: umount slave");
    if (syscall(SYS_umount2, PROP_TR_SRC, MNT_DETACH) < 0)
        FAIL("prop-tr: umount src");
    rmdir(PROP_TR_SLOT);
    rmdir(PROP_TR_SLAVE);
    rmdir(PROP_TR_SRC);
    rmdir(PROP_TR_BASE);

    printf("UNSHARE_MOUNT_NS_TRANSITIVE_SLAVE_PASSED\n");
}

/* ── original fork-based test ──────────────────────────────────────── */

static void child_body(int ready_fd, int release_fd) {
    if (unshare(CLONE_NEWNS) < 0)
        FAIL("unshare(CLONE_NEWNS)");
    PASS("child unshared mount namespace");

    if (mount(SOURCE, TARGET, "none", MS_BIND, NULL) < 0)
        FAIL("bind mount source onto target");
    if (access(TARGET_MARKER, F_OK) < 0)
        FAIL("child sees marker through namespace-local mount");
    PASS("child sees namespace-local bind mount");

    write_all(ready_fd, "R", 1, "signal child mount ready");
    read_one(release_fd, "wait parent setns check");

    if (umount2(TARGET, MNT_DETACH) < 0)
        FAIL("detach child bind mount");
    exit(0);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("================================================\n");
    printf("  TEST: unshare/setns(CLONE_NEWNS) mount view\n");
    printf("================================================\n");

    test_unprivileged_mount_operations();

    /* Scenario 1: clone(CLONE_FS) + child unshare(CLONE_NEWNS) → parent
       must NOT see child's namespace-local bind mount. */
    test_clone_fs_unshare_new_ns();

    test_clone_ns_shared_peer();
    test_clone_ns_master_slave();
    test_clone_ns_transitive_slave_propagation();

    /* Scenario 2: fork() + child unshare(CLONE_NEWNS) + setns. */
    prepare_tree();

    int ready_pipe[2];
    int release_pipe[2];
    if (pipe(ready_pipe) < 0)
        FAIL("pipe ready");
    if (pipe(release_pipe) < 0)
        FAIL("pipe release");

    pid_t child = fork();
    if (child < 0)
        FAIL("fork");

    if (child == 0) {
        close(ready_pipe[0]);
        close(release_pipe[1]);
        child_body(ready_pipe[1], release_pipe[0]);
    }

    close(ready_pipe[1]);
    close(release_pipe[0]);
    read_one(ready_pipe[0], "wait child mount ready");

    if (access(TARGET_MARKER, F_OK) == 0) {
        errno = EEXIST;
        FAIL("parent original namespace must not see child bind mount");
    }
    if (errno != ENOENT)
        FAIL("check original namespace target marker absence");
    PASS("parent original namespace does not see child bind mount");

    char ns_path[64];
    snprintf(ns_path, sizeof(ns_path), "/proc/%d/ns/mnt", child);
    int nsfd = open(ns_path, O_RDONLY | O_CLOEXEC);
    if (nsfd < 0)
        FAIL("open child /proc/<pid>/ns/mnt");
    if (xsetns(nsfd, CLONE_NEWNS) < 0)
        FAIL("setns child mount namespace");
    if (close(nsfd) < 0)
        FAIL("close nsfd");
    if (access(TARGET_MARKER, F_OK) < 0)
        FAIL("parent sees child bind mount after setns");
    PASS("setns(CLONE_NEWNS) switches to target mount view");

    write_all(release_pipe[1], "D", 1, "release child");

    int status;
    if (waitpid(child, &status, 0) < 0)
        FAIL("waitpid child");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
        FAIL("child exited non-zero");
    PASS("child exited cleanly");

    printf("UNSHARE_MOUNT_NS_ALL_PASSED\n");
    return 0;
}
