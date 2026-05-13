/*
 * bug-proc-stat-diskstats: BusyBox iostat needs Linux-style /proc/stat and
 * /proc/diskstats records to print its avg-cpu section.
 */
#define _GNU_SOURCE

#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

enum {
    BUF_SIZE = 8192,
};

static int read_file(const char *path, char *buf, size_t buf_size)
{
    int fd = open(path, O_RDONLY);
    size_t total = 0;

    if (fd < 0) {
        printf("FAIL: open(%s): %s\n", path, strerror(errno));
        return -1;
    }

    while (total + 1 < buf_size) {
        ssize_t n = read(fd, buf + total, buf_size - 1 - total);
        if (n < 0) {
            printf("FAIL: read(%s): %s\n", path, strerror(errno));
            close(fd);
            return -1;
        }
        if (n == 0) {
            break;
        }
        total += (size_t)n;
    }

    buf[total] = '\0';
    close(fd);
    return 0;
}

static int count_cpu_fields(const char *stat)
{
    const char *p = stat;
    int fields = 0;

    if (strncmp(p, "cpu ", 4) != 0) {
        return 0;
    }

    p += 4;
    while (*p != '\0' && *p != '\n') {
        while (isspace((unsigned char)*p)) {
            p++;
        }
        if (!isdigit((unsigned char)*p)) {
            break;
        }
        fields++;
        while (isdigit((unsigned char)*p)) {
            p++;
        }
    }

    return fields;
}

static int diskstats_has_real_device_line(const char *diskstats)
{
    const char *line = diskstats;

    while (*line != '\0') {
        int major;
        int minor;
        char name[64];
        unsigned long long reads;
        unsigned long long reads_merged;
        unsigned long long sectors_read;
        unsigned long long read_ms;
        unsigned long long writes;
        unsigned long long writes_merged;
        unsigned long long sectors_written;
        unsigned long long write_ms;
        unsigned long long in_flight;
        unsigned long long io_ms;
        unsigned long long weighted_ms;
        int fields;

        fields = sscanf(line,
                        " %d %d %63s %llu %llu %llu %llu %llu %llu %llu %llu %llu %llu %llu",
                        &major,
                        &minor,
                        name,
                        &reads,
                        &reads_merged,
                        &sectors_read,
                        &read_ms,
                        &writes,
                        &writes_merged,
                        &sectors_written,
                        &write_ms,
                        &in_flight,
                        &io_ms,
                        &weighted_ms);
        if (fields >= 14 && strcmp(name, "loop0") != 0 && reads > 0 && sectors_read > 0) {
            return 1;
        }

        line = strchr(line, '\n');
        if (line == NULL) {
            break;
        }
        line++;
    }

    return 0;
}

int main(void)
{
    static char stat_buf[BUF_SIZE];
    static char diskstats_buf[BUF_SIZE];
    int cpu_fields;

    printf("=== bug-proc-stat-diskstats ===\n");
    printf("Expected: /proc/stat starts with cpu counters and /proc/diskstats reports real block I/O\n\n");

    if (read_file("/proc/stat", stat_buf, sizeof(stat_buf)) != 0) {
        printf("TEST FAILED\n");
        return 1;
    }

    cpu_fields = count_cpu_fields(stat_buf);
    if (cpu_fields < 8) {
        printf("FAIL: /proc/stat cpu line has %d numeric fields, expected at least 8\n", cpu_fields);
        printf("First bytes: %.160s\n", stat_buf);
        printf("TEST FAILED\n");
        return 1;
    }
    printf("PASS: /proc/stat cpu line has %d numeric fields\n", cpu_fields);

    if (read_file("/proc/diskstats", diskstats_buf, sizeof(diskstats_buf)) != 0) {
        printf("TEST FAILED\n");
        return 1;
    }

    if (!diskstats_has_real_device_line(diskstats_buf)) {
        printf("FAIL: /proc/diskstats lacks a real device row with nonzero read counters\n");
        printf("First bytes: %.160s\n", diskstats_buf);
        printf("TEST FAILED\n");
        return 1;
    }
    printf("PASS: /proc/diskstats has a real block device row\n");

    printf("TEST PASSED\n");
    return 0;
}
