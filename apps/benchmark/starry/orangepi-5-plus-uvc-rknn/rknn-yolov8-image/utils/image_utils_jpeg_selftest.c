#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "image_utils.h"
#include "turbojpeg.h"

enum HeaderMode {
    HEADER_OK,
    HEADER_ERROR,
    HEADER_OVERSIZED,
};

static enum HeaderMode header_mode = HEADER_OK;
static int decompress_calls = 0;

tjhandle tjInitDecompress(void)
{
    return &header_mode;
}

int tjDecompressHeader3(
    tjhandle handle,
    const unsigned char *jpegBuf,
    unsigned long jpegSize,
    int *width,
    int *height,
    int *jpegSubsamp,
    int *jpegColorspace)
{
    (void)handle;
    (void)jpegBuf;
    (void)jpegSize;
    if (header_mode == HEADER_ERROR) {
        return -1;
    }
    *width = header_mode == HEADER_OVERSIZED ? 50000 : 1;
    *height = header_mode == HEADER_OVERSIZED ? 50000 : 1;
    *jpegSubsamp = TJSAMP_420;
    *jpegColorspace = TJCS_YCbCr;
    return 0;
}

int tjDecompress2(
    tjhandle handle,
    const unsigned char *jpegBuf,
    unsigned long jpegSize,
    unsigned char *dstBuf,
    int width,
    int pitch,
    int height,
    int pixelFormat,
    int flags)
{
    (void)handle;
    (void)jpegBuf;
    (void)jpegSize;
    (void)width;
    (void)pitch;
    (void)height;
    (void)pixelFormat;
    (void)flags;
    decompress_calls++;
    dstBuf[0] = 1;
    dstBuf[1] = 2;
    dstBuf[2] = 3;
    return 0;
}

int tjDestroy(tjhandle handle)
{
    (void)handle;
    return 0;
}

int tjGetErrorCode(tjhandle handle)
{
    (void)handle;
    return 1;
}

char *tjGetErrorStr(void)
{
    return "selftest stub";
}

static int require(int condition, const char *message)
{
    if (!condition) {
        fprintf(stderr, "FAIL: %s\n", message);
        return 1;
    }
    return 0;
}

static int write_fixture(const char *path)
{
    static const unsigned char jpeg_bytes[] = {0xff, 0xd8, 0xff, 0xd9};
    FILE *file = fopen(path, "wb");
    if (file == NULL) {
        return -1;
    }
    int ok = fwrite(jpeg_bytes, 1, sizeof(jpeg_bytes), file) == sizeof(jpeg_bytes);
    return fclose(file) == 0 && ok ? 0 : -1;
}

int main(void)
{
    const char *fixture = "image_utils_jpeg_selftest_input.jpg";
    image_buffer_t image;

    memset(&image, 0, sizeof(image));
    if (require(read_image("image_utils_jpeg_selftest_missing.jpg", &image) == -1,
                "reject a missing JPEG without dereferencing a null FILE")) {
        return 1;
    }
    if (write_fixture(fixture) != 0) {
        fprintf(stderr, "FAIL: create JPEG fixture\n");
        return 1;
    }

    header_mode = HEADER_ERROR;
    memset(&image, 0, sizeof(image));
    if (require(read_image(fixture, &image) == -1, "propagate a JPEG header failure")) {
        remove(fixture);
        return 1;
    }

    header_mode = HEADER_OVERSIZED;
    decompress_calls = 0;
    memset(&image, 0, sizeof(image));
    if (require(read_image(fixture, &image) == -1, "reject an overflowing RGB layout") ||
        require(decompress_calls == 0, "do not decompress an overflowing RGB layout")) {
        remove(fixture);
        return 1;
    }

    header_mode = HEADER_OK;
    decompress_calls = 0;
    memset(&image, 0, sizeof(image));
    if (require(read_image(fixture, &image) == 0, "accept a valid JPEG layout") ||
        require(decompress_calls == 1, "decompress a valid JPEG exactly once") ||
        require(image.width == 1 && image.height == 1, "store JPEG dimensions") ||
        require(image.width_stride == 3 && image.height_stride == 1, "store RGB strides") ||
        require(image.size == 3, "store RGB byte size") ||
        require(image.virt_addr != NULL, "allocate an RGB output buffer")) {
        free(image.virt_addr);
        remove(fixture);
        return 1;
    }

    free(image.virt_addr);
    remove(fixture);
    puts("PASS image_utils_jpeg_selftest");
    return 0;
}
