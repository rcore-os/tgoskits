#include <cstdio>
#include <cstdlib>
#include <cstring>

#include "turbojpeg.h"
#include "uvc_capture.h"

namespace {

int header_width;
int header_height;
int decompress_calls;
int destroy_calls;
int decoder_state;

int require(bool condition, const char *message)
{
    if (!condition) {
        std::fprintf(stderr, "FAIL: %s\n", message);
        return 1;
    }
    return 0;
}

LatestFrame mjpeg_frame()
{
    LatestFrame frame;
    frame.format = UVC_FRAME_FORMAT_MJPEG;
    frame.data = {0xff, 0xd8, 0xff, 0xd9};
    return frame;
}

void reset_decoder(int width, int height)
{
    header_width = width;
    header_height = height;
    decompress_calls = 0;
    destroy_calls = 0;
}

}  // namespace

extern "C" tjhandle tjInitDecompress(void)
{
    return reinterpret_cast<tjhandle>(&decoder_state);
}

extern "C" int tjDecompressHeader3(
    tjhandle handle,
    const unsigned char *jpeg_buf,
    unsigned long jpeg_size,
    int *width,
    int *height,
    int *jpeg_subsamp,
    int *jpeg_colorspace)
{
    (void)handle;
    (void)jpeg_buf;
    (void)jpeg_size;
    *width = header_width;
    *height = header_height;
    *jpeg_subsamp = TJSAMP_420;
    *jpeg_colorspace = TJCS_YCbCr;
    return 0;
}

extern "C" int tjDecompress2(
    tjhandle handle,
    const unsigned char *jpeg_buf,
    unsigned long jpeg_size,
    unsigned char *dst_buf,
    int width,
    int pitch,
    int height,
    int pixel_format,
    int flags)
{
    (void)handle;
    (void)jpeg_buf;
    (void)jpeg_size;
    (void)width;
    (void)pitch;
    (void)height;
    (void)pixel_format;
    (void)flags;
    ++decompress_calls;
    dst_buf[0] = 1;
    dst_buf[1] = 2;
    dst_buf[2] = 3;
    return 0;
}

extern "C" int tjDestroy(tjhandle handle)
{
    (void)handle;
    ++destroy_calls;
    return 0;
}

extern "C" int tjGetErrorCode(tjhandle handle)
{
    (void)handle;
    return 0;
}

extern "C" char *tjGetErrorStr(void)
{
    return const_cast<char *>("selftest stub");
}

int main()
{
    LatestFrame frame = mjpeg_frame();
    image_buffer_t image;
    std::memset(&image, 0, sizeof(image));

    // This produces 2^32 + 131072 RGB bytes. The old int arithmetic wrapped
    // to a 128 KiB allocation and called tjDecompress2; the checked layout
    // must reject it before allocating or decoding.
    reset_decoder(65536, 21846);
    if (require(frame_to_image(frame, &image) == -1,
                "reject an MJPEG header whose RGB size exceeds image_buffer_t") ||
        require(decompress_calls == 0, "do not decode an overflowing MJPEG layout") ||
        require(destroy_calls == 1, "destroy the decoder after rejecting the header") ||
        require(image.virt_addr == NULL, "leave the output image unallocated after rejection")) {
        return 1;
    }

    reset_decoder(1, 1);
    if (require(frame_to_image(frame, &image) == 0, "decode a representable MJPEG layout") ||
        require(decompress_calls == 1, "decode a representable MJPEG layout exactly once") ||
        require(destroy_calls == 1, "destroy the decoder after a successful decode") ||
        require(image.width == 1 && image.height == 1, "store decoded dimensions") ||
        require(image.width_stride == 3 && image.height_stride == 1, "store RGB strides") ||
        require(image.size == 3, "store the RGB byte size") ||
        require(image.virt_addr != NULL, "allocate a decoded RGB buffer")) {
        std::free(image.virt_addr);
        return 1;
    }

    std::free(image.virt_addr);
    std::puts("PASS uvc_capture_mjpeg_selftest");
    return 0;
}
