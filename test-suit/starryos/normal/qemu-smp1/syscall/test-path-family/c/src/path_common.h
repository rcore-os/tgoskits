#pragma once

#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#define PATH_FAMILY_BASE "/tmp/starry_syscall_test_path_family"
#define PATH_TEST_DROP_UID 65534

enum path_perm_kind {
    PATH_PERM_DIR = 0,
    PATH_PERM_FILE = 1,
};

enum path_drop_probe_status {
    PATH_DROP_PROBE_OK = 0,
    PATH_DROP_PROBE_NEED_ROOT = 1,
    PATH_DROP_PROBE_SETUID_FAILED = 2,
};

struct path_perm_matrix_entry {
    const char *name;
    mode_t mode;
    int kind;
};

int path_setup_perm_matrix_at(
    int dirfd,
    const struct path_perm_matrix_entry *entries,
    size_t count
);
void path_cleanup_perm_matrix_at(
    int dirfd,
    const struct path_perm_matrix_entry *entries,
    size_t count
);
int path_run_as_dropped_user(int *out_value, int (*probe_fn)(void *), void *arg);

static inline void path_join(char *out, size_t out_size, const char *rel)
{
    snprintf(out, out_size, "%s/%s", PATH_FAMILY_BASE, rel);
}

static inline int open_base_dir(void)
{
    int dfd = open(PATH_FAMILY_BASE, O_RDONLY | O_DIRECTORY);
    CHECK(dfd >= 0, "open(BASE) as dirfd");
    return dfd;
}
