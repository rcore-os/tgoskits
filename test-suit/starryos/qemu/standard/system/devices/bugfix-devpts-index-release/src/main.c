#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <poll.h>
#include <pty.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Well past the 16 devpts indexes, so a leak cannot hide behind a larger table. */
#define ROUNDS 40

int main(void)
{
    TEST_START("closing a pty returns its devpts index");

    int opened = 0;
    for (int i = 0; i < ROUNDS; i++) {
        int master = posix_openpt(O_RDWR | O_NOCTTY);
        if (master < 0)
            break;
        close(master);
        opened++;
    }
    CHECK_RET(opened, ROUNDS, "a master opened and closed 40 times never runs out of indexes");

    opened = 0;
    for (int i = 0; i < ROUNDS; i++) {
        int master = -1;
        int slave = -1;
        if (openpty(&master, &slave, NULL, NULL, NULL) != 0)
            break;
        close(slave);
        close(master);
        opened++;
    }
    CHECK_RET(opened, ROUNDS, "a pty pair opened and closed 40 times never runs out of indexes");

    int master = -1;
    int slave = -1;
    char name[64];
    CHECK_RET(openpty(&master, &slave, name, NULL, NULL), 0, "openpty");
    CHECK_RET(close(master), 0, "close the master while the slave stays open");
    CHECK_ERR(access(name, F_OK), ENOENT, "the slave node disappears with the master");

    int other_master = -1;
    int other_slave = -1;
    char other_name[64];
    CHECK_RET(openpty(&other_master, &other_slave, other_name, NULL, NULL), 0,
              "open another pty while the old slave is still open");
    CHECK(strcmp(name, other_name) != 0, "an index whose slave is still open is not handed out again");
    close(other_slave);
    close(other_master);
    CHECK_RET(close(slave), 0, "close the old slave");

    opened = 0;
    for (int i = 0; i < ROUNDS; i++) {
        int m = -1;
        int s = -1;
        if (openpty(&m, &s, NULL, NULL, NULL) != 0)
            break;
        close(s);
        close(m);
        opened++;
    }
    CHECK_RET(opened, ROUNDS, "indexes stay available after a slave outlived its master");

    /* A freed index goes to the next pty, and its name must reach that pty rather than the old one. */
    int first_master = -1;
    int first_slave = -1;
    char first_name[64];
    CHECK_RET(openpty(&first_master, &first_slave, first_name, NULL, NULL), 0, "open a pty");
    close(first_slave);
    close(first_master);
    int next_master = -1;
    int next_slave = -1;
    char next_name[64];
    CHECK_RET(openpty(&next_master, &next_slave, next_name, NULL, NULL), 0, "open the next pty");
    CHECK(strcmp(first_name, next_name) == 0, "the freed index is handed out again");
    CHECK_RET(write(next_slave, "x", 1), 1, "write through the slave openpty opened by name");
    struct pollfd pfd = {.fd = next_master, .events = POLLIN};
    CHECK_RET(poll(&pfd, 1, 1000), 1, "the byte reaches the new master");
    char byte = 0;
    CHECK(read(next_master, &byte, 1) == 1 && byte == 'x', "the new master reads it");
    close(next_slave);
    close(next_master);

    TEST_DONE();
}
