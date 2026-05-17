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

#include "image_utils.h"
#include "yolov8.h"

static void print_usage(const char *argv0)
{
    printf("Usage: %s [model_path] <image_path> [label_path]\n", argv0);
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
    const char *image_path = NULL;
    const char *label_path = "model/coco_80_labels_list.txt";

    if (argc == 2) {
        image_path = argv[1];
    } else if (argc == 3) {
        model_path = argv[1];
        image_path = argv[2];
    } else if (argc == 4) {
        model_path = argv[1];
        image_path = argv[2];
        label_path = argv[3];
    } else {
        print_usage(argv[0]);
        return 2;
    }

    printf("YOLOv8 Image Detection\n");
    printf("======================\n");
    printf("model: %s\n", model_path);
    printf("image: %s\n", image_path);
    printf("label: %s\n", label_path);

    image_buffer_t src_image;
    memset(&src_image, 0, sizeof(src_image));

    int ret = read_image(image_path, &src_image);
    if (ret != 0) {
        printf("read_image fail! ret=%d image_path=%s\n", ret, image_path);
        return 1;
    }
    printf("read_image success: width=%d height=%d format=%d size=%d\n",
           src_image.width,
           src_image.height,
           src_image.format,
           src_image.size);

    rknn_app_context_t app_ctx;
    memset(&app_ctx, 0, sizeof(app_ctx));

    ret = init_post_process(label_path);
    if (ret != 0) {
        printf("init_post_process fail! ret=%d label_path=%s\n", ret, label_path);
        if (src_image.virt_addr != NULL) {
            free(src_image.virt_addr);
        }
        return 1;
    }

    ret = init_yolov8_model(model_path, &app_ctx);
    if (ret != 0) {
        printf("init_yolov8_model fail! ret=%d model_path=%s\n", ret, model_path);
        deinit_post_process();
        if (src_image.virt_addr != NULL) {
            free(src_image.virt_addr);
        }
        return 1;
    }
    printf("init_yolov8_model success!\n");
    printf("Model info: width=%d, height=%d, channel=%d\n",
           app_ctx.model_width,
           app_ctx.model_height,
           app_ctx.model_channel);

    object_detect_result_list od_results;
    memset(&od_results, 0, sizeof(od_results));

    ret = inference_yolov8_model(&app_ctx, &src_image, &od_results);
    if (ret != 0) {
        printf("inference_yolov8_model fail! ret=%d\n", ret);
    } else {
        printf("inference_yolov8_model success!\n");
        print_detection_results(&od_results);
    }

    int release_ret = release_yolov8_model(&app_ctx);
    if (release_ret != 0) {
        printf("release_yolov8_model fail! ret=%d\n", release_ret);
    }

    deinit_post_process();

    if (src_image.virt_addr != NULL) {
        free(src_image.virt_addr);
    }

    if (ret == 0 && release_ret == 0) {
        printf("UVC_RKNN_IMAGE_DONE\n");
        return 0;
    }
    return 1;
}
