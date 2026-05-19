#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <poll.h>
#include <sys/wait.h>
#include <termios.h>

// Put terminal in raw mode
void enable_raw_mode() {
    struct termios raw;
    tcgetattr(STDIN_FILENO, &raw);
    raw.c_lflag &= ~(ECHO | ICANON | IEXTEN | ISIG);
    raw.c_iflag &= ~(BRKINT | ICRNL | INPCK | ISTRIP | IXON);
    raw.c_cflag |= (CS8);
    raw.c_cc[VMIN] = 0;
    raw.c_cc[VTIME] = 0;
    tcsetattr(STDIN_FILENO, TCSAFLUSH, &raw);
}

int main() {
    printf("[TEST] Putting terminal in RAW mode...\n");
    enable_raw_mode(); // <--- THE SUSPECT

    printf("[TEST] Starting internal watchdog for stdin poll()...\n");
    pid_t pid = fork();

    if (pid == 0) {
        struct pollfd fds[1];
        fds[0].fd = STDIN_FILENO;
        fds[0].events = POLLIN;

        int ret = poll(fds, 1, 1000);
        if (ret == 0) exit(0); // Success
        else exit(2); // Error or unexpected data
    } else {
        int status;
        for (int i = 0; i < 3; i++) {
            if (waitpid(pid, &status, WNOHANG) == pid) {
                if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
                    printf("[PASS] Child poll() timed out correctly.\n");
                    printf("TEST PASSED\n");
                    return 0;
                }
                printf("[FAIL] Unexpected exit code.\n");
                printf("TEST FAILED\n");
                return 1;
            }
            sleep(1);
        }
        
        printf("[FAIL] Watchdog triggered! Child hung indefinitely in RAW mode.\n");
        printf("TEST FAILED\n");
        kill(pid, 9);
        return 1;
    }
}