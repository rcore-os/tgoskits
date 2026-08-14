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
    UvcRgbImageLayout layout = {};
    size_t yuyv_size = 0;
    const int max_rgb_width = std::numeric_limits<int>::max() / 3;
    if (require(uvc_rgb_image_layout(320, 240, &layout), "accept a normal frame") ||
        require(layout.row_stride == 960, "calculate RGB row stride") ||
        require(layout.size == 230400, "calculate RGB byte size") ||
        require(uvc_yuyv_image_layout(320, 240, &layout, &yuyv_size),
                "accept a YUYV frame with complete pixel pairs") ||
        require(yuyv_size == 153600, "calculate YUYV source size") ||
        require(!uvc_yuyv_image_layout(1, 1, &layout, &yuyv_size),
                "reject an odd-width YUYV frame") ||
        require(!uvc_yuyv_image_layout(1, 2, &layout, &yuyv_size),
                "do not pair YUYV pixels across rows") ||
        require(uvc_yuyv_image_layout(max_rgb_width, 1, &layout, &yuyv_size),
                "accept the largest representable one-row YUYV layout") ||
        require(yuyv_size == static_cast<size_t>(max_rgb_width) * 2,
                "calculate the largest representable YUYV source size") ||
        require(!uvc_rgb_image_layout(0, 240, &layout), "reject zero width") ||
        require(!uvc_rgb_image_layout(320, 0, &layout), "reject zero height") ||
        require(!uvc_rgb_image_layout(50000, 50000, &layout),
                "reject an RGB size that exceeds image_buffer_t") ||
        require(!uvc_rgb_image_layout(std::numeric_limits<int>::max(), 1, &layout),
                "reject a row stride that exceeds image_buffer_t")) {
        return 1;
    }

    puts("PASS uvc_capture_layout_selftest");
    return 0;
}
