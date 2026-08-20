#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#define DIRECTORY_PATH "/tmp/getdents64_bounded_buffer"
#define FILE_PATH DIRECTORY_PATH "/entry"
#define BUFFER_SIZE 4097

int main(void)
{
    unsigned char buffer[BUFFER_SIZE];
    int file = -1;
    int dir = -1;
    int result = 1;

    unlink(FILE_PATH);
    rmdir(DIRECTORY_PATH);
    if (mkdir(DIRECTORY_PATH, 0700) != 0) {
        perror("mkdir");
        goto cleanup;
    }
    file = open(FILE_PATH, O_CREAT | O_WRONLY | O_TRUNC, 0600);
    if (file < 0) {
        perror("open entry");
        goto cleanup;
    }
    close(file);
    file = -1;

    dir = open(DIRECTORY_PATH, O_RDONLY | O_DIRECTORY);
    if (dir < 0) {
        perror("open directory");
        goto cleanup;
    }

    memset(buffer, 0xA5, sizeof(buffer));
    long bytes = syscall(SYS_getdents64, dir, buffer, sizeof(buffer));
    if (bytes <= 0 || (size_t)bytes >= sizeof(buffer)) {
        fprintf(stderr, "getdents64 returned %ld (errno=%d)\n", bytes, errno);
        goto cleanup;
    }
    if (buffer[bytes] != 0xA5) {
        fputs("getdents64 modified bytes beyond its return value\n", stderr);
        goto cleanup;
    }

    errno = 0;
    if (syscall(SYS_getdents64, -1, buffer, sizeof(buffer)) != -1 || errno != EBADF) {
        fprintf(stderr, "invalid getdents64 fd returned errno=%d, expected EBADF\n", errno);
        goto cleanup;
    }

    result = 0;

cleanup:
    if (dir >= 0) {
        close(dir);
    }
    if (file >= 0) {
        close(file);
    }
    unlink(FILE_PATH);
    rmdir(DIRECTORY_PATH);
    return result;
}
