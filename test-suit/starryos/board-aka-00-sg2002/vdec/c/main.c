#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#include "fixtures.h"

/* Independent 64-bit userspace view of the Sophgo VDEC wire ABI. */
struct channel_attr {
    int32_t payload_type, video_mode;
    uint32_t width, height, stream_size, frame_size, frame_count, video_attr[3];
};
struct stream {
    uint32_t length, padding;
    uint64_t pts;
    uint8_t end_of_frame, end_of_stream, display, padding2[5];
    uint64_t address;
};
struct wrapper { uint64_t pointer; int32_t timeout; uint32_t padding; };
struct frame {
    uint32_t width, height;
    int32_t pixel_format, bayer_format, video_format, compress_mode, dynamic_range, color_gamut;
    uint32_t stride[3], padding;
    uint64_t physical[3], virtual_address[3];
    uint32_t length[3];
    int16_t offset_top, offset_bottom, offset_left, offset_right;
    uint32_t time_ref;
    uint64_t pts, private_data;
    uint32_t flag, padding2;
};
struct frame_info { struct frame frame; uint32_t pool, padding; };
_Static_assert(sizeof(struct channel_attr) == 40, "channel ABI");
_Static_assert(sizeof(struct stream) == 32, "stream ABI");
_Static_assert(sizeof(struct wrapper) == 16, "wrapper ABI");
_Static_assert(sizeof(struct frame_info) == 152, "frame ABI");

#define CHECK(condition) do { \
    if (!(condition)) { \
        fprintf(stderr, "STARRY_VDEC_FAILED line=%d check=%s errno=%d\n", \
                __LINE__, #condition, errno); \
        exit(EXIT_FAILURE); \
    } \
} while (0)

static void decode_frame(int fd, const unsigned char *jpeg, size_t jpeg_len, uint64_t pts) {
    struct stream stream = {
        .length = jpeg_len, .pts = pts, .end_of_frame = 1, .display = 1,
        .address = (uintptr_t)jpeg,
    };
    struct wrapper input = {.pointer = (uintptr_t)&stream, .timeout = 1000};
    CHECK(syscall(SYS_ioctl, fd, 0x5639, &input) == 0); /* SEND_STREAM */

    struct frame_info info = {0};
    struct wrapper output = {.pointer = (uintptr_t)&info, .timeout = 1000};
    CHECK(syscall(SYS_ioctl, fd, 0x563a, &output) == 0); /* GET_FRAME */
    const struct frame *f = &info.frame;
    CHECK(f->width == 32 && f->height == 32 && f->pixel_format == 13);
    CHECK(f->video_format == 0 && f->compress_mode == 0 && f->pts == pts);
    CHECK(f->stride[0] == 32 && f->stride[1] == 16 && f->stride[2] == 16);
    CHECK(f->length[0] == 1024 && f->length[1] == 256 && f->length[2] == 256);
    CHECK(f->physical[0] != 0);
    CHECK(f->physical[1] == f->physical[0] + 1024);
    CHECK(f->physical[2] == f->physical[0] + 1280);

    /* Split reads cross row and plane boundaries, including a short last read. */
    unsigned char decoded[1536];
    size_t offset = 0;
    while (offset < sizeof(decoded)) {
        unsigned char chunk[13];
        long n = syscall(SYS_pread64, fd, chunk, sizeof(chunk), offset);
        size_t expected = sizeof(decoded) - offset;
        if (expected > sizeof(chunk)) expected = sizeof(chunk);
        CHECK(n == (long)expected);
        memcpy(decoded + offset, chunk, expected);
        offset += expected;
    }
    unsigned char byte;
    CHECK(syscall(SYS_pread64, fd, &byte, 1, sizeof(decoded)) == 0);

    /* Fixtures contain constant 16x16 YCbCr blocks. A tolerance of two covers
     * JPEG IDCT rounding; all sampling formats must reconstruct the same image. */
    for (unsigned plane = 0; plane < 3; ++plane) {
        unsigned side = plane == 0 ? 32 : 16;
        unsigned start = plane == 0 ? 0 : (plane == 1 ? 1024 : 1280);
        for (unsigned y = 0; y < side; ++y) {
            for (unsigned x = 0; x < side; ++x) {
                int expected = plane == 0 ? 96 + 32 * (x / 16)
                    : plane == 1 ? 64 + 64 * (x / 8) : 96 + 64 * (y / 8);
                int actual = decoded[start + y * side + x];
                if (abs(actual - expected) > 2) {
                    fprintf(stderr, "pixel plane=%u x=%u y=%u actual=%d expected=%d\n",
                            plane, x, y, actual, expected);
                    CHECK(0);
                }
            }
        }
    }
    CHECK(syscall(SYS_ioctl, fd, 0x563b, &info) == 0); /* RELEASE_FRAME */
    CHECK(syscall(SYS_pread64, fd, &byte, 1, 0) == -1 && errno == EINVAL);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    int fd = open("/dev/cvi_vc_dec0", O_RDWR);
    CHECK(fd >= 0);
    struct channel_attr attr = {
        .payload_type = 1002, .video_mode = 1, .width = 32, .height = 32,
        .stream_size = 65536, .frame_size = 4096, .frame_count = 1,
    };
    CHECK(syscall(SYS_ioctl, fd, 0x562f, &attr) == 0); /* CREATE_CHN */
    CHECK(syscall(SYS_ioctl, fd, 0x5633, 0) == 0); /* START_RECV_STREAM */
    const struct { const char *name; const unsigned char *jpeg; size_t len; } cases[] = {
        {"420", yuv420, sizeof(yuv420)},
        {"422h", yuv422h, sizeof(yuv422h)},
        {"422v", yuv422v, sizeof(yuv422v)},
        {"444", yuv444, sizeof(yuv444)},
    };
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i) {
        printf("VDEC_BEGIN format=%s\n", cases[i].name);
        decode_frame(fd, cases[i].jpeg, cases[i].len, 123 + i);
        printf("VDEC_OK format=%s\n", cases[i].name);
    }
    CHECK(syscall(SYS_ioctl, fd, 0x5634, 0) == 0); /* STOP_RECV_STREAM */
    /* Channel capacity describes the compact output, not native DMA storage. */
    attr.frame_size = 1536;
    CHECK(syscall(SYS_ioctl, fd, 0x5632, &attr) == 0); /* SET_CHN_ATTR */
    CHECK(syscall(SYS_ioctl, fd, 0x5633, 0) == 0);
    decode_frame(fd, yuv444, sizeof(yuv444), 127);
    CHECK(syscall(SYS_ioctl, fd, 0x5634, 0) == 0);
    CHECK(syscall(SYS_ioctl, fd, 0x5630, 0) == 0); /* DESTROY_CHN */
    CHECK(close(fd) == 0);
    puts("STARRY_VDEC_PASSED");
    return 0;
}
