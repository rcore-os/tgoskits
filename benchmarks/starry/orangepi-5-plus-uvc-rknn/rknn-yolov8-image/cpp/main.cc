// Copyright (c) 2023 by Rockchip Electronics Co., Ltd. All Rights Reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <vector>

#include "image_utils.h"
#include "yolov8.h"

static void print_usage(const char *argv0)
{
    printf("Usage: %s [model_path] <image_path> [label_path]\n", argv0);
    printf("       %s --batch <model_path> <label_path> <image_path>...\n", argv0);
    printf("Default model_path: model/yolov8.rknn\n");
    printf("Default label_path: model/coco_80_labels_list.txt\n");
}

static void print_detection_results(const object_detect_result_list *od_results)
{
    printf("=== Detection Results Summary ===\n");
    printf("Detected objects: %d\n", od_results->count);

    if (od_results->count == 0) {
        printf("No objects detected\n");
        return;
    }

    for (int i = 0; i < od_results->count; i++) {
        const object_detect_result *det = &od_results->results[i];
        int box_width = det->box.right - det->box.left;
        int box_height = det->box.bottom - det->box.top;
        int center_x = det->box.left + box_width / 2;
        int center_y = det->box.top + box_height / 2;

        printf("[%d] %s\n", i + 1, coco_cls_to_name(det->cls_id));
        printf("    Confidence: %.1f%%\n", det->prop * 100);
        printf("    Position: (%d, %d, %d, %d)\n",
               det->box.left,
               det->box.top,
               det->box.right,
               det->box.bottom);
        printf("    Box Size: %dx%d\n", box_width, box_height);
        printf("    Center: (%d, %d)\n", center_x, center_y);
    }
}

int main(int argc, char **argv)
{
    const char *model_path = "model/yolov8.rknn";
    const char *label_path = "model/coco_80_labels_list.txt";
    std::vector<const char *> images;
    const bool batch = argc > 1 && strcmp(argv[1], "--batch") == 0;

    if (batch && argc >= 5) {
        model_path = argv[2];
        label_path = argv[3];
        images.assign(argv + 4, argv + argc);
    } else if (!batch && argc == 2) {
        images.push_back(argv[1]);
    } else if (!batch && (argc == 3 || argc == 4)) {
        model_path = argv[1];
        images.push_back(argv[2]);
        if (argc == 4) label_path = argv[3];
    } else {
        print_usage(argv[0]);
        return 2;
    }

    printf("YOLOv8 Image Detection\n");
    printf("model: %s\nlabel: %s\nexpected_images: %zu\n",
           model_path, label_path, images.size());

    rknn_app_context_t app_ctx;
    memset(&app_ctx, 0, sizeof(app_ctx));
    int ret = init_post_process(label_path);
    if (ret != 0) {
        printf("init_post_process fail! ret=%d label_path=%s\n", ret, label_path);
        return 1;
    }

    ret = init_yolov8_model(model_path, &app_ctx);
    if (ret != 0) {
        printf("init_yolov8_model fail! ret=%d model_path=%s\n", ret, model_path);
        deinit_post_process();
        return 1;
    }

    // Keep results until every requested image and model cleanup has succeeded.
    // An empty detection list is valid; a missing/failed inference is not.
    std::vector<object_detect_result_list> results;
    for (const char *image_path : images) {
        image_buffer_t src_image;
        memset(&src_image, 0, sizeof(src_image));
        printf("Processing image: %s\n", image_path);
        ret = read_image(image_path, &src_image);
        if (ret != 0) {
            printf("read_image fail! ret=%d image_path=%s\n", ret, image_path);
            free(src_image.virt_addr);
            break;
        }

        object_detect_result_list result;
        memset(&result, 0, sizeof(result));
        ret = inference_yolov8_model(&app_ctx, &src_image, &result);
        free(src_image.virt_addr);
        if (ret != 0) {
            printf("inference_yolov8_model fail! ret=%d image_path=%s\n", ret, image_path);
            break;
        }
        results.push_back(result);
    }

    const int release_ret = release_yolov8_model(&app_ctx);
    if (release_ret != 0) {
        printf("release_yolov8_model fail! ret=%d\n", release_ret);
    }
    if (ret != 0 || release_ret != 0 || results.empty() || results.size() != images.size()) {
        printf("UVC_RKNN_IMAGE_FAILED completed=%zu expected=%zu\n", results.size(), images.size());
        deinit_post_process();
        return 1;
    }

    for (size_t i = 0; i < results.size(); ++i) {
        printf("image: %s\n", images[i]);
        print_detection_results(&results[i]);
    }
    deinit_post_process();
    if (batch) {
        printf("UVC_RKNN_IMAGE_PASS images=%zu\n", results.size());
    } else {
        // Retain the existing single-image interface for interactive callers.
        printf("UVC_RKNN_IMAGE_DONE\n");
    }
    return 0;
}
