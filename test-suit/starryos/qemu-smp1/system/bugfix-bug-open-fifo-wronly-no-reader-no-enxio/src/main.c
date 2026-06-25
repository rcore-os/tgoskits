/*
 * bug-open-fifo-wronly-no-reader-no-enxio: FIFO open semantics.
 *
 * man 2 open §"ENXIO" (1st variant):
 *   "O_NONBLOCK | O_WRONLY is set, the named file is a FIFO, and no process
 *    has the FIFO open for reading."
 *
 * Linux behavior:
 *   - O_WRONLY|O_NONBLOCK with no reader returns -1/ENXIO.
 *   - O_WRONLY|O_NONBLOCK with an existing reader succeeds.
 *   - O_RDWR succeeds and the returned fd is both readable and writable.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#define CHECK(cond, msg) do { \
    if (!(cond)) { \
        printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno)); \
        return 1; \
    } \
    printf("PASS: %s\n", msg); \
} while (0)

int main(void)
{
    const char *fifo = "/tmp/bug_fifo_open_semantics";
    unlink(fifo);
    if (mkfifo(fifo, 0644) != 0) { perror("mkfifo"); return 1; }

    errno = 0;
    int fd = open(fifo, O_WRONLY | O_NONBLOCK);
    CHECK(fd == -1 && errno == ENXIO,
          "open(FIFO, O_WRONLY|O_NONBLOCK) no reader -> -1 ENXIO");

    int reader = open(fifo, O_RDONLY | O_NONBLOCK);
    CHECK(reader >= 0, "open(FIFO, O_RDONLY|O_NONBLOCK) creates reader");

    errno = 0;
    int writer = open(fifo, O_WRONLY | O_NONBLOCK);
    CHECK(writer >= 0,
          "open(FIFO, O_WRONLY|O_NONBLOCK) succeeds when reader exists");

    const char byte = 'x';
    CHECK(write(writer, &byte, 1) == 1, "writer writes to reader-backed FIFO");
    char out = 0;
    CHECK(read(reader, &out, 1) == 1 && out == byte,
          "reader receives byte from writer-backed FIFO");

    close(writer);
    close(reader);

    int rw = open(fifo, O_RDWR | O_NONBLOCK);
    CHECK(rw >= 0, "open(FIFO, O_RDWR|O_NONBLOCK) succeeds");
    const char self_byte = 'y';
    CHECK(write(rw, &self_byte, 1) == 1, "O_RDWR FIFO fd is writable");
    out = 0;
    CHECK(read(rw, &out, 1) == 1 && out == self_byte,
          "O_RDWR FIFO fd is readable");

    close(rw);
    if (fd >= 0) close(fd);
    unlink(fifo);
    return 0;
}
