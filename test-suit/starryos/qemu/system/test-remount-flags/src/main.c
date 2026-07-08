#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <unistd.h>

#define BUF_SIZE 65536

int main(void) {
    /* Remount /tmp with nosuid */
    if (mount(NULL, "/tmp", NULL, MS_REMOUNT | MS_NOSUID, NULL) < 0) {
        perror("remount /tmp nosuid");
        return 1;
    }

    /* Read /proc/self/mountinfo and check /tmp entry has nosuid */
    FILE *f = fopen("/proc/self/mountinfo", "r");
    if (!f) {
        fprintf(stderr, "FAIL: cannot open /proc/self/mountinfo\n");
        return 1;
    }

    char buf[BUF_SIZE];
    size_t n = fread(buf, 1, sizeof(buf) - 1, f);
    fclose(f);
    buf[n] = '\0';

    /* Find the /tmp line */
    char *saveptr = NULL;
    char *line = strtok_r(buf, "\n", &saveptr);
    int found = 0;

    while (line) {
        long mount_id, parent_id;
        int major, minor;
        char root[256], mount_point[256], options[256];
        char optional[256], fstype[64], source[64], super_opts[256];

        int fields = sscanf(line, "%ld %ld %d:%d %255s %255s %255s %255s - %63s %63s %255s",
                            &mount_id, &parent_id, &major, &minor,
                            root, mount_point, options, optional,
                            fstype, source, super_opts);

        if (fields >= 7 && strcmp(mount_point, "/tmp") == 0) {
            found = 1;
            if (strstr(options, "nosuid") == NULL) {
                fprintf(stderr, "FAIL: /tmp options='%s' missing nosuid\n", options);
                return 1;
            }
            break;
        }

        line = strtok_r(NULL, "\n", &saveptr);
    }

    if (!found) {
        fprintf(stderr, "FAIL: /tmp not found in mountinfo\n");
        return 1;
    }

    printf("TEST_REMOUNT_FLAGS_PASSED\n");
    return 0;
}
