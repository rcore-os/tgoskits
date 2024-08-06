#include <stdio.h>
#include <unistd.h>

int main() {
    printf("parent start, pid = %d!\n", getpid());
    int pid = fork();
    if (pid == 0) {
        printf("hello child process, child pid = %d!\n", getpid());
        return 100;
    } else {
        printf("hello parent process, parent pid = %d, child pid = %d!\n", getpid(), pid);
        return 0;
    }
}
