#include "uvc_capture.h"

#include <limits>

constexpr int kRgbBytesPerPixel = 3;

bool uvc_rgb_image_layout(int width, int height, UvcRgbImageLayout *layout)
{
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

bool uvc_yuyv_image_layout(
    int width,
    int height,
    UvcRgbImageLayout *layout,
    size_t *source_size)
{
    if (source_size == NULL || !uvc_rgb_image_layout(width, height, layout)) {
        return false;
    }

    // Each YUYV macropixel contains two horizontally adjacent pixels.
    if (width % 2 != 0) {
        return false;
    }

    const size_t pixel_count = static_cast<size_t>(layout->size) / kRgbBytesPerPixel;
    *source_size = pixel_count * 2;
    return true;
}
