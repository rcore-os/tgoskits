#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int check_entry(const char *name)
{
    char path[128];
    char buf[32];
    snprintf(path, sizeof(path), "/proc/sys/user/%s", name);

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        fprintf(stderr, "FAIL: cannot open %s\n", path);
        return -1;
    }
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0) {
        fprintf(stderr, "FAIL: empty read from %s\n", path);
        return -1;
    }
    buf[n] = '\0';

    long val = 0;
    if (sscanf(buf, "%ld", &val) != 1 || val <= 0) {
        fprintf(stderr, "FAIL: %s has invalid value: %s", path, buf);
        return -1;
    }
    printf("INFO: %s = %ld\n", name, val);
    return 0;
}

int main(void)
{
    const char *entries[] = {
        "max_user_namespaces",
        "max_mnt_namespaces",
        "max_pid_namespaces",
        "max_net_namespaces",
        "max_uts_namespaces",
        "max_ipc_namespaces",
        "max_cgroup_namespaces",
    };

    for (size_t i = 0; i < sizeof(entries) / sizeof(entries[0]); i++) {
        if (check_entry(entries[i]) != 0) {
            return 1;
        }
    }

    printf("TEST_MAX_NS_ENTRIES_PASSED\n");
    return 0;
}
