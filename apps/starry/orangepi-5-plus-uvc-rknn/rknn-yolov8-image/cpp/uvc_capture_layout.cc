#include "uvc_capture.h"

#include <limits>

bool mjpeg_rgb_image_layout(int width, int height, MjpegRgbImageLayout *layout)
{
    constexpr int kRgbBytesPerPixel = 3;
    const int max_size = std::numeric_limits<int>::max();
    if (layout == NULL || width <= 0 || height <= 0 || width > max_size / kRgbBytesPerPixel) {
        return false;
    }

    const int row_stride = width * kRgbBytesPerPixel;
    if (height > max_size / row_stride) {
        return false;
    }

    layout->width = width;
    layout->height = height;
    layout->row_stride = row_stride;
    layout->size = row_stride * height;
    return true;
}
