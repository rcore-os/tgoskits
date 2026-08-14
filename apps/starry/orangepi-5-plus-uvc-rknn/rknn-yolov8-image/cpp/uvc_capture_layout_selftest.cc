#include <limits>
#include <stdio.h>

#include "uvc_capture.h"

static int require(bool condition, const char *message)
{
    if (!condition) {
        fprintf(stderr, "FAIL: %s\n", message);
        return 1;
    }
    return 0;
}

int main()
{
    MjpegRgbImageLayout layout = {};
    if (require(mjpeg_rgb_image_layout(320, 240, &layout), "accept a normal frame") ||
        require(layout.row_stride == 960, "calculate RGB row stride") ||
        require(layout.size == 230400, "calculate RGB byte size") ||
        require(!mjpeg_rgb_image_layout(0, 240, &layout), "reject zero width") ||
        require(!mjpeg_rgb_image_layout(320, 0, &layout), "reject zero height") ||
        require(!mjpeg_rgb_image_layout(50000, 50000, &layout),
                "reject an RGB size that exceeds image_buffer_t") ||
        require(!mjpeg_rgb_image_layout(std::numeric_limits<int>::max(), 1, &layout),
                "reject a row stride that exceeds image_buffer_t")) {
        return 1;
    }

    puts("PASS uvc_capture_layout_selftest");
    return 0;
}
