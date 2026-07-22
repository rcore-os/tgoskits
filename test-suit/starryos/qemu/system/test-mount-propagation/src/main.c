#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define BUF_SIZE 65536

struct mi_entry {
    long mount_id;
    long parent_id;
    char mount_point[256];
};

struct propagation_tree {
    const char *source_parent;
    const char *peer_parent;
    const char *slave_parent;
    const char *source_child;
    const char *peer_child;
    const char *slave_child;
};

static int read_mountinfo(char *buf, size_t size) {
    FILE *file = fopen("/proc/self/mountinfo", "r");
    if (!file)
        return -1;
    size_t count = fread(buf, 1, size - 1, file);
    fclose(file);
    buf[count] = '\0';
    return (int)count;
}

static int find_mount_entry(const char *buf, const char *path,
                            struct mi_entry *entry) {
    char *copy = strdup(buf);
    if (!copy)
        return 0;
    char *save = NULL;
    for (char *line = strtok_r(copy, "\n", &save); line;
         line = strtok_r(NULL, "\n", &save)) {
        char root[256];
        char options[256];
        int major;
        int minor;
        if (sscanf(line, "%ld %ld %d:%d %255s %255s %255s",
                   &entry->mount_id, &entry->parent_id, &major, &minor, root,
                   entry->mount_point, options) == 7 &&
            strcmp(entry->mount_point, path) == 0) {
            free(copy);
            return 1;
        }
    }
    free(copy);
    return 0;
}

static int mount_exists(const char *path) {
    char buf[BUF_SIZE];
    struct mi_entry entry;
    return read_mountinfo(buf, sizeof(buf)) >= 0 &&
           find_mount_entry(buf, path, &entry);
}

static int make_dir(const char *path) {
    if (mkdir(path, 0755) == 0 || errno == EEXIST)
        return 0;
    fprintf(stderr, "FAIL: mkdir %s: %s\n", path, strerror(errno));
    return 1;
}

static int setup_propagation_tree(struct propagation_tree *tree,
                                  const char *prefix) {
    static char paths[6][128];
    snprintf(paths[0], sizeof(paths[0]), "/%s_source", prefix);
    snprintf(paths[1], sizeof(paths[1]), "/%s_peer", prefix);
    snprintf(paths[2], sizeof(paths[2]), "/%s_slave", prefix);
    snprintf(paths[3], sizeof(paths[3]), "/%s_source/slot", prefix);
    snprintf(paths[4], sizeof(paths[4]), "/%s_peer/slot", prefix);
    snprintf(paths[5], sizeof(paths[5]), "/%s_slave/slot", prefix);
    tree->source_parent = paths[0];
    tree->peer_parent = paths[1];
    tree->slave_parent = paths[2];
    tree->source_child = paths[3];
    tree->peer_child = paths[4];
    tree->slave_child = paths[5];

    if (make_dir(tree->source_parent) || make_dir(tree->peer_parent) ||
        make_dir(tree->slave_parent))
        return 1;
    if (mount("tmpfs", tree->source_parent, "tmpfs", 0, NULL) < 0) {
        perror("mount propagation source parent");
        return 1;
    }
    if (syscall(SYS_mount, NULL, tree->source_parent, NULL, MS_SHARED, NULL) <
        0) {
        perror("make propagation source parent shared");
        return 1;
    }
    if (mount(tree->source_parent, tree->peer_parent, NULL, MS_BIND, NULL) <
        0) {
        perror("bind propagation peer parent");
        return 1;
    }
    if (mount(tree->source_parent, tree->slave_parent, NULL, MS_BIND, NULL) <
        0) {
        perror("bind propagation slave parent");
        return 1;
    }
    if (syscall(SYS_mount, NULL, tree->slave_parent, NULL, MS_SLAVE, NULL) <
        0) {
        perror("make propagation slave parent slave");
        return 1;
    }
    if (make_dir(tree->source_child))
        return 1;
    if (mount("tmpfs", tree->source_child, "tmpfs", 0, NULL) < 0) {
        perror("mount propagated child");
        return 1;
    }
    if (!mount_exists(tree->source_child) || !mount_exists(tree->peer_child) ||
        !mount_exists(tree->slave_child)) {
        fprintf(stderr, "FAIL: corresponding propagated children are missing\n");
        return 1;
    }
    return 0;
}

static int snapshot_ids(const struct propagation_tree *tree, long ids[3]) {
    char buf[BUF_SIZE];
    struct mi_entry entries[3];
    if (read_mountinfo(buf, sizeof(buf)) < 0 ||
        !find_mount_entry(buf, tree->source_child, &entries[0]) ||
        !find_mount_entry(buf, tree->peer_child, &entries[1]) ||
        !find_mount_entry(buf, tree->slave_child, &entries[2]))
        return 1;
    for (size_t index = 0; index < 3; ++index)
        ids[index] = entries[index].mount_id;
    return 0;
}

static int expect_ebusy_unchanged(const struct propagation_tree *tree,
                                  const char *reason,
                                  const char *nested_child) {
    long before[3];
    long after[3];
    if (snapshot_ids(tree, before)) {
        fprintf(stderr, "FAIL: %s snapshot failed before umount\n", reason);
        return 1;
    }
    errno = 0;
    if (syscall(SYS_umount2, tree->source_child, 0) == 0 || errno != EBUSY) {
        fprintf(stderr, "FAIL: %s expected EBUSY, got errno=%d (%s)\n",
                reason, errno, errno ? strerror(errno) : "success");
        return 1;
    }
    if (snapshot_ids(tree, after) || memcmp(before, after, sizeof(before)) != 0 ||
        !mount_exists(tree->source_parent) || !mount_exists(tree->peer_parent) ||
        !mount_exists(tree->slave_parent) ||
        (nested_child && !mount_exists(nested_child))) {
        fprintf(stderr, "FAIL: %s changed topology after EBUSY\n", reason);
        return 1;
    }
    return 0;
}

static int cleanup_tree(const struct propagation_tree *tree) {
    if (mount_exists(tree->source_child) &&
        umount2(tree->source_child, MNT_DETACH) < 0)
        return 1;
    if (mount_exists(tree->slave_parent) && umount(tree->slave_parent) < 0)
        return 1;
    if (mount_exists(tree->peer_parent) && umount(tree->peer_parent) < 0)
        return 1;
    if (mount_exists(tree->source_parent) && umount(tree->source_parent) < 0)
        return 1;
    rmdir(tree->slave_parent);
    rmdir(tree->peer_parent);
    rmdir(tree->source_parent);
    return 0;
}

static int test_nullable_shared_and_independent_peer(void) {
    const char *source = "/prop_nullable_source";
    const char *peer = "/prop_nullable_peer";
    if (make_dir(source) || make_dir(peer))
        return 1;
    if (mount("tmpfs", source, "tmpfs", 0, NULL) < 0)
        return 1;
    if (syscall(SYS_mount, NULL, source, NULL, MS_SHARED, NULL) < 0)
        return 1;
    if (mount(source, peer, NULL, MS_BIND, NULL) < 0)
        return 1;
    if (umount(source) < 0)
        return 1;
    if (mount_exists(source) || !mount_exists(peer)) {
        fprintf(stderr, "FAIL: explicit shared peer did not remain independent\n");
        return 1;
    }
    if (umount(peer) < 0)
        return 1;
    rmdir(peer);
    rmdir(source);
    return 0;
}

static int test_corresponding_mount_identity(void) {
    struct propagation_tree tree;
    if (setup_propagation_tree(&tree, "prop_identity"))
        return 1;
    char buf[BUF_SIZE];
    struct mi_entry source;
    struct mi_entry peer;
    struct mi_entry slave;
    if (read_mountinfo(buf, sizeof(buf)) < 0 ||
        !find_mount_entry(buf, tree.source_child, &source) ||
        !find_mount_entry(buf, tree.peer_child, &peer) ||
        !find_mount_entry(buf, tree.slave_child, &slave) ||
        source.mount_id == peer.mount_id || source.mount_id == slave.mount_id ||
        peer.mount_id == slave.mount_id ||
        source.parent_id == peer.parent_id || source.parent_id == slave.parent_id ||
        peer.parent_id == slave.parent_id) {
        fprintf(stderr, "FAIL: corresponding mount identity is not local\n");
        return 1;
    }
    return cleanup_tree(&tree);
}

static int test_normal_unmount_busy(void) {
    struct propagation_tree tree;
    if (setup_propagation_tree(&tree, "prop_busy"))
        return 1;

    int file_fd = open("/prop_busy_peer/slot/open-file",
                       O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (file_fd < 0 || expect_ebusy_unchanged(&tree, "peer open fd", NULL))
        return 1;
    close(file_fd);

    char original_cwd[256];
    if (!getcwd(original_cwd, sizeof(original_cwd)) ||
        chdir(tree.peer_child) < 0 ||
        expect_ebusy_unchanged(&tree, "peer cwd", NULL) ||
        chdir(original_cwd) < 0)
        return 1;

    const char *nested = "/prop_busy_peer/slot/nested";
    if (make_dir(nested) || mount("tmpfs", nested, "tmpfs", 0, NULL) < 0 ||
        expect_ebusy_unchanged(&tree, "peer nested mount", nested) ||
        umount(nested) < 0)
        return 1;

    int ready[2];
    int release[2];
    if (pipe(ready) < 0 || pipe(release) < 0)
        return 1;
    pid_t holder = fork();
    if (holder < 0)
        return 1;
    if (holder == 0) {
        char token;
        close(ready[0]);
        close(release[1]);
        if (chroot(tree.slave_child) < 0 || chdir("/") < 0)
            _exit(2);
        token = 'R';
        if (write(ready[1], &token, 1) != 1 ||
            read(release[0], &token, 1) != 1)
            _exit(3);
        _exit(0);
    }
    close(ready[1]);
    close(release[0]);
    char token;
    if (read(ready[0], &token, 1) != 1 || token != 'R' ||
        expect_ebusy_unchanged(&tree, "slave task root", NULL))
        return 1;
    token = 'X';
    if (write(release[1], &token, 1) != 1)
        return 1;
    close(ready[0]);
    close(release[1]);
    int status;
    if (waitpid(holder, &status, 0) != holder || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0)
        return 1;

    if (cleanup_tree(&tree))
        return 1;

    struct propagation_tree admitted;
    if (setup_propagation_tree(&admitted, "prop_admitted"))
        return 1;
    errno = 0;
    int unmount_result = umount(admitted.source_child);
    int source_exists = mount_exists(admitted.source_child);
    int peer_exists = mount_exists(admitted.peer_child);
    int slave_exists = mount_exists(admitted.slave_child);
    if (unmount_result < 0 || source_exists || peer_exists || slave_exists) {
        fprintf(stderr,
                "FAIL: admitted normal unmount result=%d errno=%d source=%d "
                "peer=%d slave=%d\n",
                unmount_result, errno, source_exists, peer_exists,
                slave_exists);
        return 1;
    }
    return cleanup_tree(&admitted);
}

static int test_lazy_detach(void) {
    struct propagation_tree tree;
    if (setup_propagation_tree(&tree, "prop_detach"))
        return 1;

    const char *nested = "/prop_detach_peer/slot/nested";
    if (make_dir(nested) || mount("tmpfs", nested, "tmpfs", 0, NULL) < 0)
        return 1;
    int file_fd = open("/prop_detach_slave/slot/old-file",
                       O_CREAT | O_RDWR | O_TRUNC, 0644);
    int dir_fd = open(tree.peer_child, O_RDONLY | O_DIRECTORY);
    char original_cwd[256];
    const char payload[] = "lazy-reference";
    if (file_fd < 0 || dir_fd < 0 ||
        write(file_fd, payload, sizeof(payload)) != (ssize_t)sizeof(payload) ||
        !getcwd(original_cwd, sizeof(original_cwd)) ||
        chdir(tree.source_child) < 0)
        return 1;

    if (syscall(SYS_umount2, tree.source_child, MNT_DETACH) < 0)
        return 1;
    if (mount_exists(tree.source_child) || mount_exists(tree.peer_child) ||
        mount_exists(tree.slave_child) || mount_exists(nested) ||
        !mount_exists(tree.source_parent) || !mount_exists(tree.peer_parent) ||
        !mount_exists(tree.slave_parent)) {
        fprintf(stderr, "FAIL: lazy detach removed the wrong topology\n");
        return 1;
    }
    char read_buf[sizeof(payload)];
    struct stat stat_buf;
    if (lseek(file_fd, 0, SEEK_SET) < 0 ||
        read(file_fd, read_buf, sizeof(read_buf)) != (ssize_t)sizeof(read_buf) ||
        memcmp(read_buf, payload, sizeof(payload)) != 0 || fstat(file_fd, &stat_buf) < 0 ||
        fchdir(dir_fd) < 0 || chdir(original_cwd) < 0)
        return 1;
    close(dir_fd);
    close(file_fd);
    return cleanup_tree(&tree);
}

int main(void) {
    if (test_nullable_shared_and_independent_peer() ||
        test_corresponding_mount_identity() || test_normal_unmount_busy() ||
        test_lazy_detach())
        return 1;
    printf("TEST_MOUNT_PROPAGATION_PASSED\n");
    return 0;
}
