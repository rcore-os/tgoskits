// Linux Guest PID 1 helper for the P1/P2 smoke path.

#define _DEFAULT_SOURCE

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>

static void ensure_device(const char *path, unsigned major, unsigned minor) {
    unlink(path);
    (void)mknod(path, S_IFCHR | 0600, makedev(major, minor));
}

int main(void) {
    ensure_device("/dev/console", 5, 1);
    ensure_device("/dev/null", 1, 3);
    ensure_device("/dev/kmsg", 1, 11);
    int console = open("/dev/console", O_RDWR | O_NOCTTY);
    if (console >= 0) {
        dup2(console, STDIN_FILENO);
        dup2(console, STDOUT_FILENO);
        dup2(console, STDERR_FILENO);
        if (console > STDERR_FILENO) {
            close(console);
        }
    }
    puts("TASK2_INIT_START");
    fflush(stdout);
    if (access("/sys/class/net/eth0", F_OK) == 0) {
        int result = system("ifconfig eth0 10.0.42.1 netmask 255.255.255.0 up");
        printf("TASK2_IFCONFIG_RC=%d\n", result);
        fflush(stdout);
        if (system("udp_probe recv 4242 &") != 0) {
            puts("TASK2_PROBE_START_FAILED");
            return 1;
        }
        puts("TASK2_UDP_RECV_STARTED");
    } else {
        puts("TASK2_NO_ETH0");
    }
    fflush(stdout);
    for (;;) {
        pause();
    }
}
