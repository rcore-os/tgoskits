/*
 * Focused StarryOS PTY diagnostic: test /dev/ptmx open + grantpt/unlockpt.
 *
 * Nix uses open("/dev/ptmx") → grantpt() → unlockpt() → ptsname() → open(slave)
 * to create pseudo-terminals for builder processes. If any of these fail,
 * the builder process never starts.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define PASS(msg)  printf("PTY_PASS: %s\n", msg)
#define FAIL(msg)  printf("PTY_FAIL: %s (errno=%d: %s)\n", msg, errno, strerror(errno))
#define DIAG(msg)  printf("PTY_DIAG: %s\n", msg)

int main(void)
{
    /* 1. Check if /dev/ptmx exists */
    struct stat st;
    if (stat("/dev/ptmx", &st) != 0) {
        FAIL("stat /dev/ptmx");
        printf("PTY_DIAG_COMPLETE\n");
        return 1;
    }
    if (!S_ISCHR(st.st_mode)) {
        printf("PTY_FAIL: /dev/ptmx is not a character device (mode=0%o)\n", st.st_mode);
        printf("PTY_DIAG_COMPLETE\n");
        return 1;
    }
    PASS("stat /dev/ptmx is char dev");

    /* 2. Open /dev/ptmx */
    int master = open("/dev/ptmx", O_RDWR | O_NOCTTY);
    if (master < 0) {
        FAIL("open /dev/ptmx O_RDWR|O_NOCTTY");
        /* Also try without O_NOCTTY */
        master = open("/dev/ptmx", O_RDWR);
        if (master >= 0) {
            DIAG("open /dev/ptmx O_RDWR succeeded (O_NOCTTY failed)");
        } else {
            printf("PTY_FAIL: open /dev/ptmx O_RDWR also failed (errno=%d: %s)\n",
                   errno, strerror(errno));
            printf("PTY_DIAG_COMPLETE\n");
            return 1;
        }
    }
    PASS("open /dev/ptmx");

    /* 3. grantpt() */
    if (grantpt(master) != 0) {
        FAIL("grantpt");
        close(master);
        printf("PTY_DIAG_COMPLETE\n");
        return 1;
    }
    PASS("grantpt");

    /* 4. unlockpt() */
    if (unlockpt(master) != 0) {
        FAIL("unlockpt");
        close(master);
        printf("PTY_DIAG_COMPLETE\n");
        return 1;
    }
    PASS("unlockpt");

    /* 5. ptsname() */
    char *slave_name = ptsname(master);
    if (slave_name == NULL) {
        FAIL("ptsname");
        close(master);
        printf("PTY_DIAG_COMPLETE\n");
        return 1;
    }
    printf("PTY_PASS: ptsname = %s\n", slave_name);

    /* 6. Open the slave */
    int slave = open(slave_name, O_RDWR | O_NOCTTY);
    if (slave < 0) {
        /* Try without O_NOCTTY */
        slave = open(slave_name, O_RDWR);
    }
    if (slave < 0) {
        FAIL("open slave pty");
        close(master);
        printf("PTY_DIAG_COMPLETE\n");
        return 1;
    }
    PASS("open slave pty");

    /* 7. Check /dev/pts/ directory */
    if (stat("/dev/pts/", &st) != 0) {
        DIAG("stat /dev/pts/ failed — devpts may not be mounted");
    } else {
        PASS("stat /dev/pts/ exists");
    }

    close(slave);
    close(master);
    printf("PTY_ALL_PASSED\n");
    return 0;
}
