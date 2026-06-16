/*
 * bug-open-unix-socket-no-enxio: open() on a UNIX domain socket file must ENXIO.
 *
 * man 2 open §"ENXIO" (3rd variant): "The file is a UNIX domain socket."
 *
 * Linux behavior: open(unix_sock_file, O_RDONLY) → -1 ENXIO.
 * StarryOS bug: returns valid fd (treats socket file like regular).
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

int main(void)
{
    const char *sock_path = "/tmp/bug_unix_sock_enxio";
    unlink(sock_path);

    int s = socket(AF_UNIX, SOCK_STREAM, 0);
    if (s < 0) { perror("socket"); return 1; }
    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    strncpy(addr.sun_path, sock_path, sizeof(addr.sun_path) - 1);
    if (bind(s, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        perror("bind"); close(s); return 1;
    }

    errno = 0;
    int fd = open(sock_path, O_RDONLY);
    int ok = (fd == -1 && errno == ENXIO);
    if (ok) {
        printf("PASS: open(unix_socket) -> -1 ENXIO\n");
    } else {
        printf("FAIL: expected -1 ENXIO, got fd=%d errno=%d (%s)\n",
               fd, errno, strerror(errno));
    }
    if (fd >= 0) close(fd);
    close(s);
    unlink(sock_path);
    return ok ? 0 : 1;
}
