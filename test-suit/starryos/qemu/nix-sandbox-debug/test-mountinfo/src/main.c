#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define BUF_SIZE 65536

int main(void) {
    FILE *f = fopen("/proc/self/mountinfo", "r");
    if (!f) {
        fprintf(stderr, "FAIL: cannot open /proc/self/mountinfo\n");
        return 1;
    }

    char buf[BUF_SIZE];
    size_t total = fread(buf, 1, sizeof(buf) - 1, f);
    fclose(f);
    buf[total] = '\0';

    if (total == 0) {
        fprintf(stderr, "FAIL: /proc/self/mountinfo is empty\n");
        return 1;
    }

    int line_count = 0;
    char *saveptr = NULL;
    char *line = strtok_r(buf, "\n", &saveptr);
    int root_found = 0;

    while (line) {
        line_count++;
        long mount_id, parent_id;
        int major, minor;
        char root[256], mount_point[256], options[256];
        char optional[256], fstype[64], source[64], super_opts[256];

        /* Format: mount_id parent_id major:minor root mount_point options optional_fields - fstype source super_options */
        int n = sscanf(line, "%ld %ld %d:%d %255s %255s %255s %255s - %63s %63s %255s",
                       &mount_id, &parent_id, &major, &minor,
                       root, mount_point, options, optional,
                       fstype, source, super_opts);

        if (n < 7) {
            fprintf(stderr, "FAIL: line %d parse error (got %d fields): %s\n", line_count, n, line);
            return 1;
        }

        /* Check mount_id is positive */
        if (mount_id <= 0) {
            fprintf(stderr, "FAIL: line %d mount_id=%ld <= 0\n", line_count, mount_id);
            return 1;
        }

        /* Check root mount: parent_id == mount_id */
        if (strcmp(mount_point, "/") == 0) {
            root_found = 1;
            if (parent_id != mount_id) {
                fprintf(stderr, "FAIL: root mount parent_id=%ld != mount_id=%ld\n", parent_id, mount_id);
                return 1;
            }
        }

        /* Check options starts with rw or ro */
        if (strncmp(options, "rw", 2) != 0 && strncmp(options, "ro", 2) != 0) {
            fprintf(stderr, "FAIL: line %d options='%s' doesn't start with rw/ro\n", line_count, options);
            return 1;
        }

        line = strtok_r(NULL, "\n", &saveptr);
    }

    if (line_count < 3) {
        fprintf(stderr, "FAIL: only %d mount entries, expected at least 3\n", line_count);
        return 1;
    }

    if (!root_found) {
        fprintf(stderr, "FAIL: root mount (/) not found in mountinfo\n");
        return 1;
    }

    printf("TEST_MOUNTINFO_PASSED\n");
    return 0;
}
