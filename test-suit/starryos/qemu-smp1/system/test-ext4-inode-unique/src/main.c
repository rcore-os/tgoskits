#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define FILE_COUNT 2048
#define PATH_LEN   128

struct file_identity {
    char path[PATH_LEN];
    int fd;
    struct stat st_path;
    struct stat st_fd;
};

static int fail_errno(const char *what)
{
    printf("EXT4_INODE_UNIQUE_FAILED: %s: errno=%d (%s)\n", what, errno, strerror(errno));
    return 1;
}

static int same_identity(const struct stat *a, const struct stat *b)
{
    return a->st_dev == b->st_dev && a->st_ino == b->st_ino;
}

static void print_identity(const char *label, const struct stat *st)
{
    printf("EXT4_INODE_UNIQUE_DIAG: %s dev=%llu ino=%llu mode=%o nlink=%lu\n",
           label,
           (unsigned long long)st->st_dev,
           (unsigned long long)st->st_ino,
           (unsigned int)st->st_mode,
           (unsigned long)st->st_nlink);
}

static int create_and_stat(struct file_identity *id)
{
    id->fd = open(id->path, O_CREAT | O_EXCL | O_RDWR, 0600);
    if (id->fd < 0) {
        return fail_errno("open O_CREAT|O_EXCL");
    }

    if (write(id->fd, id->path, strlen(id->path)) < 0) {
        return fail_errno("write marker");
    }

    if (fsync(id->fd) != 0) {
        return fail_errno("fsync marker");
    }

    if (stat(id->path, &id->st_path) != 0) {
        return fail_errno("stat path");
    }

    if (fstat(id->fd, &id->st_fd) != 0) {
        return fail_errno("fstat fd");
    }

    if (!same_identity(&id->st_path, &id->st_fd)) {
        printf("EXT4_INODE_UNIQUE_FAILED: stat/fstat identity mismatch for %s\n", id->path);
        print_identity("stat", &id->st_path);
        print_identity("fstat", &id->st_fd);
        return 1;
    }

    close(id->fd);
    id->fd = -1;

    if (id->st_path.st_ino < 16 || id->st_path.st_ino % 64 == 0) {
        print_identity(id->path, &id->st_path);
    }
    return 0;
}

int main(void)
{
    const char *store_dir = "/nix/store";
    const char *dir = "/nix/store/ext4-inode-unique";
    struct file_identity *files = calloc(FILE_COUNT, sizeof(*files));

    if (files == NULL) {
        return fail_errno("calloc file identities");
    }

    if (mkdir("/nix", 0755) != 0 && errno != EEXIST) {
        return fail_errno("mkdir /nix");
    }
    if (mkdir(store_dir, 0755) != 0 && errno != EEXIST) {
        return fail_errno("mkdir /nix/store");
    }

    for (size_t i = 0; i < FILE_COUNT; i++) {
        snprintf(files[i].path, sizeof(files[i].path), "%s/file-%04zu.lock", dir, i);
        files[i].fd = -1;
        unlink(files[i].path);
    }
    rmdir(dir);

    if (mkdir(dir, 0700) != 0) {
        return fail_errno("mkdir test dir");
    }

    for (size_t i = 0; i < FILE_COUNT; i++) {
        if (create_and_stat(&files[i]) != 0) {
            return 1;
        }
    }

    for (size_t i = 0; i < FILE_COUNT; i++) {
        for (size_t j = i + 1; j < FILE_COUNT; j++) {
            if (same_identity(&files[i].st_path, &files[j].st_path)) {
                printf("EXT4_INODE_UNIQUE_FAILED: duplicate identity for %s and %s\n",
                       files[i].path,
                       files[j].path);
                print_identity(files[i].path, &files[i].st_path);
                print_identity(files[j].path, &files[j].st_path);
                return 1;
            }
        }
    }

    free(files);

    printf("EXT4_INODE_UNIQUE_ALL_PASSED\n");
    return 0;
}
