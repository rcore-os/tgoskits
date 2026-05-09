#include <errno.h>
#include <stdio.h>
#include <string.h>

static int check_contains(const char *text, const char *needle)
{
    if (strstr(text, needle) != NULL) {
        printf("PASS: /proc/net/arp contains %s\n", needle);
        return 0;
    }

    printf("FAIL: /proc/net/arp missing %s\n", needle);
    return 1;
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

    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }

    printf("SOME TESTS FAILED\n");
    return 1;
}
