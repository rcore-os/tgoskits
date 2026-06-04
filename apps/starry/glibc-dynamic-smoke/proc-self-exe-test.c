#include <stdio.h>
#include <unistd.h>
#include <limits.h>

int main(void) {
    char buf[PATH_MAX];
    ssize_t len = readlink("/proc/self/exe", buf, sizeof(buf) - 1);
    if (len < 0) {
        perror("readlink /proc/self/exe");
        return 1;
    }
    buf[len] = '\0';
    printf("/proc/self/exe -> %s\n", buf);
    return 0;
}
