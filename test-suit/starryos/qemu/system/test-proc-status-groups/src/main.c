#define _GNU_SOURCE

#include <errno.h>
#include <grp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int read_proc_groups(gid_t *groups, size_t capacity)
{
    FILE *file = fopen("/proc/self/status", "r");
    if (file == NULL) {
        return -1;
    }

    char line[256];
    while (fgets(line, sizeof(line), file) != NULL) {
        if (strncmp(line, "Groups:", strlen("Groups:")) != 0) {
            continue;
        }

        char *cursor = line + strlen("Groups:");
        size_t count = 0;
        for (;;) {
            while (*cursor == ' ' || *cursor == '\t') {
                ++cursor;
            }
            if (*cursor == '\0' || *cursor == '\n') {
                fclose(file);
                return (int)count;
            }

            char *end = NULL;
            errno = 0;
            unsigned long group = strtoul(cursor, &end, 10);
            if (errno == ERANGE || end == cursor || count == capacity) {
                fclose(file);
                errno = EINVAL;
                return -1;
            }
            groups[count++] = (gid_t)group;
            cursor = end;
        }
    }

    fclose(file);
    errno = ENOENT;
    return -1;
}

static int check_groups(const gid_t *expected, size_t expected_count)
{
    size_t capacity = expected_count == 0 ? 1 : expected_count;
    gid_t *actual = calloc(capacity, sizeof(*actual));
    if (actual == NULL) {
        return -1;
    }

    int count = read_proc_groups(actual, expected_count);
    if (count < 0 || (size_t)count != expected_count) {
        free(actual);
        return -1;
    }
    if (expected_count == 0) {
        free(actual);
        return 0;
    }
    int matches = memcmp(actual, expected, expected_count * sizeof(*expected)) == 0;
    free(actual);
    return matches ? 0 : -1;
}

static int check_current_groups(void)
{
    int count = getgroups(0, NULL);
    if (count < 0) {
        return -1;
    }

    size_t capacity = count == 0 ? 1 : (size_t)count;
    gid_t *groups = calloc(capacity, sizeof(*groups));
    if (groups == NULL) {
        return -1;
    }
    if (count > 0 && getgroups(count, groups) != count) {
        free(groups);
        return -1;
    }

    int result = check_groups(groups, (size_t)count);
    free(groups);
    return result;
}

static int child_main(void)
{
    const gid_t expected[] = {100, 200, 300};
    if (setgroups(sizeof(expected) / sizeof(expected[0]), expected) != 0) {
        return errno == EPERM ? 77 : 1;
    }
    if (check_groups(expected, sizeof(expected) / sizeof(expected[0])) != 0) {
        return 2;
    }
    if (setgroups(0, NULL) != 0) {
        return 3;
    }
    return check_groups(NULL, 0) == 0 ? 0 : 4;
}

int main(void)
{
    if (check_current_groups() != 0) {
        fputs("FAIL: /proc/self/status Groups differs from getgroups\n", stderr);
        return 1;
    }

    if (getuid() != 0) {
        puts("PROC_STATUS_GROUPS_OK: initial groups (mutation requires root)");
        return 0;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        _exit(child_main());
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)) {
        fputs("FAIL: group-check child did not exit normally\n", stderr);
        return 1;
    }
    if (WEXITSTATUS(status) == 77) {
        puts("PROC_STATUS_GROUPS_OK: initial groups (CAP_SETGID is unavailable)");
        return 0;
    }
    if (WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: group-check child exited with %d\n", WEXITSTATUS(status));
        return 1;
    }

    puts("PROC_STATUS_GROUPS_OK");
    return 0;
}
