#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>

int main(void) {
    struct termios saved;
    if (tcgetattr(STDIN_FILENO, &saved) != 0) {
        perror("tcgetattr");
        return 1;
    }

    struct termios raw = saved;
    raw.c_lflag &= ~(ECHO | ICANON | IEXTEN | ISIG);
    raw.c_iflag &= ~(BRKINT | ICRNL | INPCK | ISTRIP | IXON);
    raw.c_cflag |= CS8;
    raw.c_cc[VMIN] = 1;
    raw.c_cc[VTIME] = 0;
    if (tcsetattr(STDIN_FILENO, TCSAFLUSH, &raw) != 0) {
        perror("tcsetattr");
        return 1;
    }

    const char query[] = "\033[6n";
    if (write(STDOUT_FILENO, query, sizeof(query) - 1) != (ssize_t)(sizeof(query) - 1)) {
        perror("write");
        tcsetattr(STDIN_FILENO, TCSAFLUSH, &saved);
        return 1;
    }

    struct pollfd pfd = {
        .fd = STDIN_FILENO,
        .events = POLLIN,
    };
    int poll_ret = poll(&pfd, 1, 1000);
    if (poll_ret < 0) {
        printf("TEST FAILED: poll returned %s\n", strerror(errno));
        tcsetattr(STDIN_FILENO, TCSAFLUSH, &saved);
        return 1;
    }
    if (poll_ret == 0) {
        printf("TEST SKIPPED: host terminal did not answer cursor report query\n");
        tcsetattr(STDIN_FILENO, TCSAFLUSH, &saved);
        return 0;
    }

    char buf[16] = {0};
    ssize_t nread = read(STDIN_FILENO, buf, sizeof(buf));
    if (nread < 0) {
        printf("TEST FAILED: read returned %s\n", strerror(errno));
        tcsetattr(STDIN_FILENO, TCSAFLUSH, &saved);
        return 1;
    }

    if (nread != 6 || memcmp(buf, "\033[1;1R", 6) != 0) {
        printf("TEST FAILED: unexpected cursor report response length=%zd\n", nread);
        tcsetattr(STDIN_FILENO, TCSAFLUSH, &saved);
        return 1;
    }

    tcsetattr(STDIN_FILENO, TCSAFLUSH, &saved);
    printf("TEST PASSED\n");
    return 0;
}
