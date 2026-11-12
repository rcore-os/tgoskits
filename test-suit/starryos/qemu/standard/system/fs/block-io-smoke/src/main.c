#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define SMOKE_PATH "/root/block-io-smoke.dat"
#define SMOKE_BYTES 4096U

static void fill_buffer(uint8_t *buf, size_t len) {
    for (size_t i = 0; i < len; i++) {
        buf[i] = (uint8_t)((i * 97U + 23U) & 0xffU);
    }
}

static uint64_t checksum_buffer(const uint8_t *buf, size_t len) {
    uint64_t sum = 0;
    for (size_t i = 0; i < len; i++) {
        sum += buf[i];
    }
    return sum;
}

static void checked_write_all(int fd, const uint8_t *buf, size_t len) {
    size_t done = 0;
    while (done < len) {
        ssize_t ret = write(fd, buf + done, len - done);
        if (ret < 0) {
            perror("write");
            exit(1);
        }
        if (ret == 0) {
            fprintf(stderr, "write returned zero\n");
            exit(1);
        }
        done += (size_t)ret;
    }
}

static void checked_read_all(int fd, uint8_t *buf, size_t len) {
    size_t done = 0;
    while (done < len) {
        ssize_t ret = read(fd, buf + done, len - done);
        if (ret < 0) {
            perror("read");
            exit(1);
        }
        if (ret == 0) {
            fprintf(stderr, "short read: %zu/%zu\n", done, len);
            exit(1);
        }
        done += (size_t)ret;
    }
}

int main(void) {
    uint8_t *write_buf = malloc(SMOKE_BYTES);
    uint8_t *read_buf = malloc(SMOKE_BYTES);
    if (write_buf == NULL || read_buf == NULL) {
        perror("malloc");
        return 1;
    }

    fill_buffer(write_buf, SMOKE_BYTES);
    memset(read_buf, 0, SMOKE_BYTES);

    int fd = open(SMOKE_PATH, O_RDWR);
    if (fd < 0) {
        perror("open");
        return 1;
    }

    printf("BLOCK_IO_SMOKE_PROGRESS phase=write bytes=%u\n", SMOKE_BYTES);
    fflush(stdout);
    checked_write_all(fd, write_buf, SMOKE_BYTES);

    printf("BLOCK_IO_SMOKE_PROGRESS phase=fsync bytes=%u\n", SMOKE_BYTES);
    fflush(stdout);
    if (fsync(fd) != 0) {
        perror("fsync");
        return 1;
    }

    if (lseek(fd, 0, SEEK_SET) < 0) {
        perror("lseek");
        return 1;
    }

    printf("BLOCK_IO_SMOKE_PROGRESS phase=read bytes=%u\n", SMOKE_BYTES);
    fflush(stdout);
    checked_read_all(fd, read_buf, SMOKE_BYTES);

    if (close(fd) != 0) {
        perror("close");
        return 1;
    }

    if (memcmp(write_buf, read_buf, SMOKE_BYTES) != 0) {
        fprintf(stderr, "readback mismatch\n");
        return 1;
    }

    printf(
        "BLOCK_IO_SMOKE_PASSED bytes=%u checksum=%llu\n",
        SMOKE_BYTES,
        (unsigned long long)checksum_buffer(read_buf, SMOKE_BYTES));
    fflush(stdout);

    free(read_buf);
    free(write_buf);
    return 0;
}
