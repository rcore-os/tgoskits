#include "path_common.h"

#include <sys/wait.h>

struct path_drop_probe_payload {
    int status;
    int value;
};

static int path_create_perm_entry_at(int dirfd, const struct path_perm_matrix_entry *entry)
{
    if (entry->kind == PATH_PERM_DIR) {
        if (mkdirat(dirfd, entry->name, entry->mode) != 0 && errno != EEXIST) {
            return -1;
        }
        if (fchmodat(dirfd, entry->name, entry->mode, 0) != 0) {
            return -1;
        }
        return 0;
    }

    if (entry->kind == PATH_PERM_FILE) {
        int fd = openat(dirfd, entry->name, O_CREAT | O_RDWR | O_TRUNC, entry->mode);
        if (fd < 0) {
            return -1;
        }
        int saved_errno = 0;
        if (fchmod(fd, entry->mode) != 0) {
            saved_errno = errno;
        }
        close(fd);
        if (saved_errno != 0) {
            errno = saved_errno;
            return -1;
        }
        return 0;
    }

    errno = EINVAL;
    return -1;
}

int path_setup_perm_matrix_at(
    int dirfd,
    const struct path_perm_matrix_entry *entries,
    size_t count
)
{
    if (entries == NULL) {
        errno = EINVAL;
        return -1;
    }

    for (size_t i = 0; i < count; i++) {
        if (path_create_perm_entry_at(dirfd, &entries[i]) != 0) {
            return -1;
        }
    }
    return 0;
}

void path_cleanup_perm_matrix_at(
    int dirfd,
    const struct path_perm_matrix_entry *entries,
    size_t count
)
{
    if (entries == NULL) {
        return;
    }

    for (size_t i = count; i > 0; i--) {
        const struct path_perm_matrix_entry *entry = &entries[i - 1];
        if (entry->kind == PATH_PERM_DIR) {
            unlinkat(dirfd, entry->name, AT_REMOVEDIR);
        } else if (entry->kind == PATH_PERM_FILE) {
            unlinkat(dirfd, entry->name, 0);
        }
    }
}

int path_run_as_dropped_user(int *out_value, int (*probe_fn)(void *), void *arg)
{
    if (out_value == NULL || probe_fn == NULL) {
        errno = EINVAL;
        return -1;
    }
    if (geteuid() != 0) {
        return PATH_DROP_PROBE_NEED_ROOT;
    }

    int pipefd[2];
    if (pipe(pipefd) != 0) {
        return -1;
    }

    pid_t pid = fork();
    if (pid < 0) {
        int saved_errno = errno;
        close(pipefd[0]);
        close(pipefd[1]);
        errno = saved_errno;
        return -1;
    }

    if (pid == 0) {
        close(pipefd[0]);
        struct path_drop_probe_payload payload = {
            .status = PATH_DROP_PROBE_OK,
            .value = 0,
        };

        if (setuid(PATH_TEST_DROP_UID) != 0) {
            payload.status = PATH_DROP_PROBE_SETUID_FAILED;
            payload.value = errno;
        } else {
            payload.value = probe_fn(arg);
        }

        (void)write(pipefd[1], &payload, sizeof(payload));
        close(pipefd[1]);
        _exit(0);
    }

    close(pipefd[1]);
    struct path_drop_probe_payload payload = {0};
    ssize_t n = read(pipefd[0], &payload, sizeof(payload));
    int saved_errno = errno;
    close(pipefd[0]);

    int status = 0;
    waitpid(pid, &status, 0);

    if (n != (ssize_t)sizeof(payload)) {
        errno = n < 0 ? saved_errno : EIO;
        return -1;
    }

    *out_value = payload.value;
    return payload.status;
}
