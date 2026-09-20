#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static char chip[256];
static int exported;
static int stale_fd = -1;
static int attribute_write(const char *name, const char *value, int expected_errno)
{
    char path[320];
    snprintf(path, sizeof(path), "%s/%s", chip, name);
    int fd = syscall(SYS_openat, AT_FDCWD, path, O_WRONLY, 0);
    if (fd < 0) { perror(path); return -1; }
    errno = 0;
    long n = syscall(SYS_write, fd, value, strlen(value));
    int saved_errno = errno;
    syscall(SYS_close, fd);
    if (expected_errno ? n != -1 || saved_errno != expected_errno : n != (long)strlen(value)) {
        fprintf(stderr, "%s write: n=%ld errno=%d expected=%d\n", path, n, saved_errno, expected_errno);
        return -1;
    }
    return 0;
}
static int attribute_read(const char *name, const char *expected)
{
    char path[320], value[64] = {0};
    snprintf(path, sizeof(path), "%s/%s", chip, name);
    int fd = syscall(SYS_openat, AT_FDCWD, path, O_RDONLY, 0);
    if (fd < 0) { perror(path); return -1; }
    long n = syscall(SYS_read, fd, value, sizeof(value)-1);
    syscall(SYS_close, fd);
    if (n < 0 || strcmp(value, expected)) {
        fprintf(stderr, "%s read: '%s' expected '%s'\n", path, value, expected);
        return -1;
    }
    return 0;
}
static int removed_attribute_rejects_read(void)
{
    char value[32];
    errno = 0;
    long n = syscall(SYS_read, stale_fd, value, sizeof(value));
    if (n != -1 || errno != ENODEV) {
        fprintf(stderr, "removed attribute read: n=%ld errno=%d\n", n, errno);
        return -1;
    }
    return 0;
}
#define CHECK(call) do { if ((call) != 0) goto failed; } while (0)
int main(int argc, char **argv)
{
    const int hardware = argc == 2 && strcmp(argv[1], "--hardware") == 0;
    DIR *dir = opendir("/sys/class/pwm");
    if (!dir) { perror("PWM class missing"); goto failed; }
    struct dirent *entry;
    while ((entry = readdir(dir))) {
        if (!strncmp(entry->d_name, "pwmchip", 7)) {
            if (snprintf(chip, sizeof(chip), "/sys/class/pwm/%s", entry->d_name) >= (int)sizeof(chip)) {
                closedir(dir); goto failed;
            }
            break;
        }
    }
    closedir(dir);
    if (!hardware) {
        if (chip[0]) { fprintf(stderr, "unexpected PWM device on QEMU\n"); goto failed; }
        puts("PWM_SYSFS_PASSED");
        return 0;
    }
    if (!chip[0]) { fprintf(stderr, "no PWM device on hardware\n"); goto failed; }
    CHECK(attribute_write("export", "0\n", 0));
    exported = 1;
    // Isolate rollback proof before polarity/duplicate-export checks so this
    // same executable fails at the cache-corruption bug on the old kernel.
    CHECK(attribute_write("pwm0/enable", "0\n", 0));
    CHECK(attribute_write("pwm0/duty_cycle", "0\n", 0));
    CHECK(attribute_write("pwm0/period", "1000000\n", 0));
    CHECK(attribute_write("pwm0/duty_cycle", "250000\n", 0));
    CHECK(attribute_write("pwm0/enable", "1\n", 0));
    CHECK(attribute_write("pwm0/duty_cycle", "2000000\n", EINVAL));
    CHECK(attribute_read("pwm0/duty_cycle", "250000\n"));
    CHECK(attribute_write("pwm0/period", "1\n", EINVAL));
    CHECK(attribute_read("pwm0/period", "1000000\n"));
    CHECK(attribute_read("pwm0/enable", "1\n"));
    CHECK(attribute_write("export", "0\n", EBUSY));
    CHECK(attribute_write("pwm0/polarity", "inversed\n", 0));
    CHECK(attribute_read("pwm0/polarity", "inversed\n"));
    CHECK(attribute_write("pwm0/polarity", "normal\n", 0));
    CHECK(attribute_write("pwm0/duty_cycle", "300000\n", 0));
    CHECK(attribute_read("pwm0/duty_cycle", "300000\n"));
    CHECK(attribute_write("pwm0/enable", "0\n", 0));
    char path[320];
    snprintf(path, sizeof(path), "%s/pwm0/period", chip);
    stale_fd = syscall(SYS_openat, AT_FDCWD, path, O_RDONLY, 0);
    if (stale_fd < 0) { perror(path); goto failed; }
    CHECK(attribute_write("unexport", "0\n", 0));
    exported = 0;
    CHECK(removed_attribute_rejects_read());
    CHECK(attribute_write("export", "0\n", 0));
    exported = 1;
    CHECK(removed_attribute_rejects_read());
    CHECK(attribute_read("pwm0/period", "1000000\n"));
    syscall(SYS_close, stale_fd);
    stale_fd = -1;
    CHECK(attribute_write("unexport", "0\n", 0));
    exported = 0;
    CHECK(attribute_write("unexport", "0\n", ENODEV));
    puts("PWM_SYSFS_PASSED");
    return 0;
failed:
    if (stale_fd >= 0) syscall(SYS_close, stale_fd);
    if (exported) {
        attribute_write("pwm0/enable", "0\n", 0);
        attribute_write("unexport", "0\n", 0);
    }
    puts("PWM_SYSFS_FAILED");
    return 1;
}
