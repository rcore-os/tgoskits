// Controlled external operations for the real image-runner entry point.
#include <cstdlib>
#include <cstring>
#include "yolov8.h"
#include "image_utils.h"

static bool fails(const char *operation) {
    const char *fault = std::getenv("IMAGE_RUNNER_FAULT");
    return fault && std::strcmp(fault, operation) == 0;
}

int init_post_process(const char *) { return fails("labels") ? -1 : 0; }
void deinit_post_process() {}
char *coco_cls_to_name(int) { static char name[] = "object"; return name; }
int init_yolov8_model(const char *, rknn_app_context_t *) { return fails("model") ? -1 : 0; }
int release_yolov8_model(rknn_app_context_t *) { return fails("release") ? -1 : 0; }
int read_image(const char *path, image_buffer_t *image) {
    image->width = image->height = 1;
    return fails("image") && std::strcmp(path, "second") == 0 ? -1 : 0;
}
int inference_yolov8_model(rknn_app_context_t *, image_buffer_t *, object_detect_result_list *) {
    static int count = 0;
    ++count;
    return fails("inference") && count == 2 ? -1 : 0;
}
