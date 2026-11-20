#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <termios.h>
#include <unistd.h>

/* RISC-V Linux UAPI layouts, without libc's larger struct termios. */
struct kernel_termios {
    unsigned int c_iflag;
    unsigned int c_oflag;
    unsigned int c_cflag;
    unsigned int c_lflag;
    unsigned char c_line;
    unsigned char c_cc[19];
};

struct kernel_termios2 {
    struct kernel_termios flags;
    unsigned int c_ispeed;
    unsigned int c_ospeed;
};

_Static_assert(sizeof(struct kernel_termios) == 36, "invalid TCGETS layout");
_Static_assert(sizeof(struct kernel_termios2) == 44, "invalid TCGETS2 layout");

#define KERNEL_BOTHER 0010000U
#define KERNEL_CBAUD 0010017U
#define KERNEL_IBSHIFT 16
#define KERNEL_CIBAUD (KERNEL_CBAUD << KERNEL_IBSHIFT)
#define KERNEL_TCGETS2 _IOR('T', 0x2a, struct kernel_termios2)
#define KERNEL_TCSETS2 _IOW('T', 0x2b, struct kernel_termios2)
#define FIXTURE_BAUDRATE 115200U

static void require(int condition, const char *message)
{
    if (!condition) {
        fprintf(stderr, "STARRY_TTY_INHERITED_BAUDRATE_FAILED: %s (errno=%d)\n",
                message, errno);
        exit(EXIT_FAILURE);
    }
}

int main(void)
{
    long serial = syscall(SYS_openat, AT_FDCWD, "/dev/ttyS0", O_RDWR | O_NOCTTY);
    require(serial >= 0, "open /dev/ttyS0");

    struct kernel_termios2 initial;
    require(syscall(SYS_ioctl, serial, KERNEL_TCGETS2, &initial) == 0,
            "TCGETS2 reads inherited settings");
    require((initial.flags.c_cflag & KERNEL_CBAUD) == B115200
                && initial.c_ispeed == FIXTURE_BAUDRATE
                && initial.c_ospeed == FIXTURE_BAUDRATE,
            "TCGETS2 reports the firmware rate in both directions");

    const unsigned int masks[] = {KERNEL_CBAUD, KERNEL_CIBAUD};
    const unsigned int codes[] = {KERNEL_BOTHER, KERNEL_BOTHER << KERNEL_IBSHIFT};
    for (size_t index = 0; index < sizeof(masks) / sizeof(masks[0]); index++) {
        struct kernel_termios legacy;
        struct kernel_termios2 observed;
        require(syscall(SYS_ioctl, serial, TCGETS, &legacy) == 0,
                "TCGETS reads legacy settings");
        legacy.c_cflag = ((initial.flags.c_cflag ^ CSTOPB) & ~masks[index]) | codes[index];
        require(syscall(SYS_ioctl, serial, TCSETS, &legacy) == 0,
                index == 0 ? "TCSETS accepts output BOTHER" : "TCSETS accepts input BOTHER");
        require(syscall(SYS_ioctl, serial, KERNEL_TCGETS2, &observed) == 0,
                "TCGETS2 reads committed line format");
        require(observed.flags.c_cflag == legacy.c_cflag
                    && observed.c_ispeed == initial.c_ispeed
                    && observed.c_ospeed == initial.c_ospeed,
                "TCSETS applies the format while preserving both numeric rates");

        struct kernel_termios2 changed = observed;
        if (index == 0)
            changed.c_ospeed = 9600;
        else
            changed.c_ispeed = 9600;
        errno = 0;
        require(syscall(SYS_ioctl, serial, KERNEL_TCSETS2, &changed) == -1 && errno == EINVAL,
                index == 0 ? "TCSETS2 rejects a new output rate" : "TCSETS2 rejects a new input rate");
        memset(&changed, 0, sizeof(changed));
        require(syscall(SYS_ioctl, serial, KERNEL_TCGETS2, &changed) == 0
                    && memcmp(&changed, &observed, sizeof(changed)) == 0,
                "rejected rate change preserves the published termios");
        require(syscall(SYS_ioctl, serial, KERNEL_TCSETS2, &initial) == 0,
                "restore the initial serial settings");
    }

    struct kernel_termios2 hangup = initial;
    struct kernel_termios2 observed;
    hangup.flags.c_cflag &= ~KERNEL_CBAUD;
    require(syscall(SYS_ioctl, serial, TCSETS, &hangup.flags) == 0,
            "TCSETS applies B0 hangup");
    require(syscall(SYS_ioctl, serial, KERNEL_TCGETS2, &observed) == 0
                && observed.flags.c_cflag == hangup.flags.c_cflag
                && observed.c_ispeed == 0 && observed.c_ospeed == 0,
            "TCGETS2 reports B0 after hangup");
    require(syscall(SYS_ioctl, serial, KERNEL_TCSETS2, &initial) == 0,
            "TCSETS2 resumes the firmware rate");
    require(syscall(SYS_ioctl, serial, KERNEL_TCGETS2, &observed) == 0
                && memcmp(&observed, &initial, sizeof(observed)) == 0,
            "leaving B0 restores the published firmware rate");

    close((int)serial);
    puts("STARRY_TTY_INHERITED_BAUDRATE_PASSED");
    return EXIT_SUCCESS;
}
