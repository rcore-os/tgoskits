#define _GNU_SOURCE

#include <errno.h>
#include <net/if.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        printf("FAIL: socket errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, "lo", IFNAMSIZ - 1);
    ifr.ifr_flags = IFF_UP | IFF_LOOPBACK | IFF_RUNNING;

    if (ioctl(fd, SIOCSIFFLAGS, &ifr) != 0) {
        printf("FAIL: SIOCSIFFLAGS lo errno=%d (%s)\n", errno, strerror(errno));
        close(fd);
        return 1;
    }

    close(fd);
    printf("TEST_SIO_SET_IFFLAGS_PASSED\n");
    return 0;
}
