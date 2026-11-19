#include <errno.h>
#include <stdio.h>
#include <string.h>

static int is_blank_line(const char *line)
{
    while (*line != '\0') {
        if (*line != ' ' && *line != '\t' && *line != '\r' && *line != '\n') {
            return 0;
        }
        line++;
    }
    return 1;
}

static int check_contains(const char *text, const char *needle)
{
    if (strstr(text, needle) != NULL) {
        printf("PASS: /proc/net/arp contains %s\n", needle);
        return 0;
    }

    printf("FAIL: /proc/net/arp missing %s\n", needle);
    return 1;
}

static int check_arp_rows(char *text)
{
    int failed = 0;
    int saw_header = 0;
    int row_count = 0;

    for (char *line = text; line != NULL; ) {
        char *next = strchr(line, '\n');
        if (next != NULL) {
            *next = '\0';
            next++;
        }

        if (is_blank_line(line)) {
            line = next;
            continue;
        }

        if (!saw_header) {
            saw_header = 1;
            line = next;
            continue;
        }

        char ip[32];
        char hw_type[32];
        char flags[32];
        char hw_addr[32];
        char mask[32];
        char device[32];
        char extra[32];
        int fields = sscanf(
            line,
            "%31s %31s %31s %31s %31s %31s %31s",
            ip,
            hw_type,
            flags,
            hw_addr,
            mask,
            device,
            extra);
        if (fields != 6) {
            printf("FAIL: malformed /proc/net/arp row: %s\n", line);
            failed++;
            line = next;
            continue;
        }

        row_count++;
        if (strcmp(ip, "10.0.2.2") == 0
            && strcmp(hw_addr, "52:54:00:12:34:56") == 0
            && strcmp(device, "eth0") == 0) {
            printf("FAIL: /proc/net/arp exposes fixed QEMU gateway stub\n");
            failed++;
        }

        line = next;
    }

    printf("PASS: /proc/net/arp data rows checked: %d\n", row_count);
    return failed;
}

int main(void)
{
    char buf[512];
    FILE *fp = fopen("/proc/net/arp", "r");
    if (fp == NULL) {
        printf("FAIL: fopen /proc/net/arp: errno=%d %s\n", errno, strerror(errno));
        return 1;
    }

    size_t nread = fread(buf, 1, sizeof(buf) - 1, fp);
    int saved_errno = errno;
    if (ferror(fp)) {
        printf("FAIL: fread /proc/net/arp: errno=%d %s\n", saved_errno, strerror(saved_errno));
        fclose(fp);
        return 1;
    }
    fclose(fp);

    buf[nread] = '\0';
    printf("/proc/net/arp content:\n%s", buf);

    int failed = 0;
    failed += check_contains(buf, "IP address");
    failed += check_contains(buf, "HW type");
    failed += check_contains(buf, "HW address");
    failed += check_contains(buf, "Device");
    failed += check_arp_rows(buf);

    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }

    printf("SOME TESTS FAILED\n");
    return 1;
}
