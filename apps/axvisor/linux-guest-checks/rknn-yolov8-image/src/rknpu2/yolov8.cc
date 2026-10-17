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
#include <math.h>
#include <stdint.h>
#include <time.h>

#include "yolov8.h"
#include "common.h"
#include "file_utils.h"
#include "image_utils.h"

static void dump_tensor_attr(rknn_tensor_attr *attr)
{
    printf("  index=%d, name=%s, n_dims=%d, dims=[%d, %d, %d, %d], n_elems=%d, size=%d, fmt=%s, type=%s, qnt_type=%s, "
           "zp=%d, scale=%f\n",
           attr->index, attr->name, attr->n_dims, attr->dims[0], attr->dims[1], attr->dims[2], attr->dims[3],
           attr->n_elems, attr->size, get_format_string(attr->fmt), get_type_string(attr->type),
           get_qnt_type_string(attr->qnt_type), attr->zp, attr->scale);
}

static double monotonic_ms()
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec * 1000.0 + (double)ts.tv_nsec / 1000000.0;
}

static bool rknn_dump_stats_enabled()
{
    const char *value = getenv("RKNN_DUMP_OUTPUT_STATS");
    return value != NULL && value[0] != '\0' && strcmp(value, "0") != 0;
}

static void dump_bytes_stats(const char *prefix, int index, const void *buf, size_t size)
{
    const unsigned char *data = (const unsigned char *)buf;
    uint64_t fnv = 1469598103934665603ULL;
    unsigned int umin = 255;
    unsigned int umax = 0;
    long long isum = 0;

    for (size_t i = 0; i < size; i++)
    {
        unsigned int value = data[i];
        if (value < umin)
        {
            umin = value;
        }
        if (value > umax)
        {
            umax = value;
        }
        isum += (signed char)data[i];
        fnv ^= value;
        fnv *= 1099511628211ULL;
    }

    printf("%s index=%d bytes=%u umin=%u umax=%u isum=%lld fnv=0x%llx first16=",
           prefix,
           index,
           (unsigned int)size,
           umin,
           umax,
           isum,
           (unsigned long long)fnv);
    size_t first = size < 16 ? size : 16;
    for (size_t i = 0; i < first; i++)
    {
        printf("%02x", data[i]);
    }
    printf("\n");
}

int init_yolov8_model(const char *model_path, rknn_app_context_t *app_ctx)
{
    int ret;
    int model_len = 0;
    char *model;
    rknn_context ctx = 0;

    // Load RKNN Model
    model_len = read_data_from_file(model_path, &model);
    if (model == NULL)
    {
        printf("load_model fail!\n");
        return -1;
    }

    ret = rknn_init(&ctx, model, model_len, 0, NULL);
    free(model);
    if (ret < 0)
    {
        printf("rknn_init fail! ret=%d\n", ret);
        return -1;
    }

    // Get Model Input Output Number
    rknn_input_output_num io_num;
    ret = rknn_query(ctx, RKNN_QUERY_IN_OUT_NUM, &io_num, sizeof(io_num));
    if (ret != RKNN_SUCC)
    {
        printf("rknn_query fail! ret=%d\n", ret);
        return -1;
    }
    printf("model input num: %d, output num: %d\n", io_num.n_input, io_num.n_output);

    // Get Model Input Info
    printf("input tensors:\n");
    rknn_tensor_attr input_attrs[io_num.n_input];
    memset(input_attrs, 0, sizeof(input_attrs));
    for (int i = 0; i < io_num.n_input; i++)
    {
        input_attrs[i].index = i;
        ret = rknn_query(ctx, RKNN_QUERY_INPUT_ATTR, &(input_attrs[i]), sizeof(rknn_tensor_attr));
        if (ret != RKNN_SUCC)
        {
            printf("rknn_query fail! ret=%d\n", ret);
            return -1;
        }
        dump_tensor_attr(&(input_attrs[i]));
    }

    // Get Model Output Info
    printf("output tensors:\n");
    rknn_tensor_attr output_attrs[io_num.n_output];
    memset(output_attrs, 0, sizeof(output_attrs));
    for (int i = 0; i < io_num.n_output; i++)
    {
        output_attrs[i].index = i;
        ret = rknn_query(ctx, RKNN_QUERY_OUTPUT_ATTR, &(output_attrs[i]), sizeof(rknn_tensor_attr));
        if (ret != RKNN_SUCC)
        {
            printf("rknn_query fail! ret=%d\n", ret);
            return -1;
        }
        dump_tensor_attr(&(output_attrs[i]));
    }

    // Set to context
    app_ctx->rknn_ctx = ctx;

    // TODO
    if (output_attrs[0].qnt_type == RKNN_TENSOR_QNT_AFFINE_ASYMMETRIC && output_attrs[0].type == RKNN_TENSOR_INT8)
    {
        app_ctx->is_quant = true;
    }
    else
    {
        app_ctx->is_quant = false;
    }

    app_ctx->io_num = io_num;
    app_ctx->input_attrs = (rknn_tensor_attr *)malloc(io_num.n_input * sizeof(rknn_tensor_attr));
    memcpy(app_ctx->input_attrs, input_attrs, io_num.n_input * sizeof(rknn_tensor_attr));
    app_ctx->output_attrs = (rknn_tensor_attr *)malloc(io_num.n_output * sizeof(rknn_tensor_attr));
    memcpy(app_ctx->output_attrs, output_attrs, io_num.n_output * sizeof(rknn_tensor_attr));

    if (input_attrs[0].fmt == RKNN_TENSOR_NCHW)
    {
        printf("model is NCHW input fmt\n");
        app_ctx->model_channel = input_attrs[0].dims[1];
        app_ctx->model_height = input_attrs[0].dims[2];
        app_ctx->model_width = input_attrs[0].dims[3];
    }
    else
    {
        printf("model is NHWC input fmt\n");
        app_ctx->model_height = input_attrs[0].dims[1];
        app_ctx->model_width = input_attrs[0].dims[2];
        app_ctx->model_channel = input_attrs[0].dims[3];
    }
    printf("model input height=%d, width=%d, channel=%d\n",
           app_ctx->model_height, app_ctx->model_width, app_ctx->model_channel);

    return 0;
}

int release_yolov8_model(rknn_app_context_t *app_ctx)
{
    if (app_ctx->input_attrs != NULL)
    {
        free(app_ctx->input_attrs);
        app_ctx->input_attrs = NULL;
    }
    if (app_ctx->output_attrs != NULL)
    {
        free(app_ctx->output_attrs);
        app_ctx->output_attrs = NULL;
    }
    if (app_ctx->rknn_ctx != 0)
    {
        rknn_destroy(app_ctx->rknn_ctx);
        app_ctx->rknn_ctx = 0;
    }
    return 0;
}

int inference_yolov8_model(rknn_app_context_t *app_ctx, image_buffer_t *img, object_detect_result_list *od_results)
{
    return inference_yolov8_model_with_thresholds(app_ctx, img, od_results, BOX_THRESH, NMS_THRESH);
}

int inference_yolov8_model_with_thresholds(rknn_app_context_t *app_ctx, image_buffer_t *img, object_detect_result_list *od_results,
                                           float box_conf_threshold, float nms_threshold)
{
    return inference_yolov8_model_with_thresholds_profile(app_ctx, img, od_results, box_conf_threshold, nms_threshold, NULL);
}

int inference_yolov8_model_with_thresholds_profile(rknn_app_context_t *app_ctx, image_buffer_t *img, object_detect_result_list *od_results,
                                                   float box_conf_threshold, float nms_threshold,
                                                   rknn_inference_profile_t *profile)
{
    int ret;
    image_buffer_t dst_img;
    letterbox_t letter_box;
    rknn_input inputs[app_ctx->io_num.n_input];
    rknn_output outputs[app_ctx->io_num.n_output];
    int bg_color = 114;
    bool outputs_acquired = false;
    bool dump_stats = false;
    const double total_start = monotonic_ms();

    if ((!app_ctx) || !(img) || (!od_results))
    {
        return -1;
    }

    memset(od_results, 0x00, sizeof(*od_results));
    memset(&letter_box, 0, sizeof(letterbox_t));
    memset(&dst_img, 0, sizeof(image_buffer_t));
    memset(inputs, 0, sizeof(inputs));
    memset(outputs, 0, sizeof(outputs));
    if (profile != NULL)
    {
        memset(profile, 0, sizeof(*profile));
        profile->perf_run_query_ret = -1;
        profile->rknn_perf_run_ms = -1.0;
    }

    // Pre Process
    double stage_start = monotonic_ms();
    dst_img.width = app_ctx->model_width;
    dst_img.height = app_ctx->model_height;
    dst_img.format = IMAGE_FORMAT_RGB888;
    dst_img.size = get_image_size(&dst_img);
    dst_img.virt_addr = (unsigned char *)malloc(dst_img.size);
    if (profile != NULL)
    {
        profile->malloc_ms = monotonic_ms() - stage_start;
    }
    if (dst_img.virt_addr == NULL)
    {
        printf("malloc buffer size:%d fail!\n", dst_img.size);
        if (profile != NULL)
        {
            profile->total_ms = monotonic_ms() - total_start;
        }
        return -1;
    }

    // letterbox
    stage_start = monotonic_ms();
    ret = convert_image_with_letterbox(img, &dst_img, &letter_box, bg_color);
    if (profile != NULL)
    {
        profile->letterbox_ms = monotonic_ms() - stage_start;
    }
    if (ret < 0)
    {
        printf("convert_image_with_letterbox fail! ret=%d\n", ret);
        goto out;
    }
    dump_stats = rknn_dump_stats_enabled();
    if (dump_stats)
    {
        printf("RKNN_LETTERBOX x_pad=%d y_pad=%d input=%dx%d resize=%dx%d scale=%.8f\n",
               letter_box.x_pad,
               letter_box.y_pad,
               letter_box.input_width,
               letter_box.input_height,
               letter_box.resize_width,
               letter_box.resize_height,
               letter_box.scale);
        dump_bytes_stats("RKNN_INPUT_STATS", 0, dst_img.virt_addr, dst_img.size);
    }

    // Set Input Data
    inputs[0].index = 0;
    inputs[0].type = RKNN_TENSOR_UINT8;
    inputs[0].fmt = RKNN_TENSOR_NHWC;
    inputs[0].size = app_ctx->model_width * app_ctx->model_height * app_ctx->model_channel;
    inputs[0].buf = dst_img.virt_addr;

    stage_start = monotonic_ms();
    ret = rknn_inputs_set(app_ctx->rknn_ctx, app_ctx->io_num.n_input, inputs);
    if (profile != NULL)
    {
        profile->inputs_set_ms = monotonic_ms() - stage_start;
    }
    if (ret < 0)
    {
        printf("rknn_input_set fail! ret=%d\n", ret);
        goto out;
    }

    // Run
    stage_start = monotonic_ms();
    ret = rknn_run(app_ctx->rknn_ctx, nullptr);
    if (profile != NULL)
    {
        profile->run_ms = monotonic_ms() - stage_start;
    }
    if (ret < 0)
    {
        printf("rknn_run fail! ret=%d\n", ret);
        goto out;
    }

    // Get Output
    memset(outputs, 0, sizeof(outputs));
    for (int i = 0; i < app_ctx->io_num.n_output; i++)
    {
        outputs[i].index = i;
        outputs[i].want_float = (!app_ctx->is_quant);
    }
    stage_start = monotonic_ms();
    ret = rknn_outputs_get(app_ctx->rknn_ctx, app_ctx->io_num.n_output, outputs, NULL);
    if (profile != NULL)
    {
        profile->outputs_get_ms = monotonic_ms() - stage_start;
    }
    if (ret < 0)
    {
        printf("rknn_outputs_get fail! ret=%d\n", ret);
        goto out;
    }
    outputs_acquired = true;
    if (profile != NULL)
    {
        rknn_perf_run perf_run;
        memset(&perf_run, 0, sizeof(perf_run));
        profile->perf_run_query_ret = rknn_query(app_ctx->rknn_ctx, RKNN_QUERY_PERF_RUN, &perf_run, sizeof(perf_run));
        if (profile->perf_run_query_ret == RKNN_SUCC)
        {
            profile->rknn_perf_run_ms = (double)perf_run.run_duration / 1000.0;
        }
    }
    if (dump_stats)
    {
        for (int i = 0; i < app_ctx->io_num.n_output; i++)
        {
            if (outputs[i].buf != NULL)
            {
                dump_bytes_stats("RKNN_OUTPUT_STATS", i, outputs[i].buf, outputs[i].size);
            }
        }
    }

    // Post Process
    stage_start = monotonic_ms();
    post_process(app_ctx, outputs, &letter_box, box_conf_threshold, nms_threshold, od_results);
    if (profile != NULL)
    {
        profile->postprocess_ms = monotonic_ms() - stage_start;
    }

    // Remeber to release rknn output
    stage_start = monotonic_ms();
    rknn_outputs_release(app_ctx->rknn_ctx, app_ctx->io_num.n_output, outputs);
    if (profile != NULL)
    {
        profile->outputs_release_ms = monotonic_ms() - stage_start;
    }
    outputs_acquired = false;

out:
    if (outputs_acquired)
    {
        stage_start = monotonic_ms();
        rknn_outputs_release(app_ctx->rknn_ctx, app_ctx->io_num.n_output, outputs);
        if (profile != NULL)
        {
            profile->outputs_release_ms = monotonic_ms() - stage_start;
        }
    }
    if (dst_img.virt_addr != NULL)
    {
        free(dst_img.virt_addr);
    }
    if (profile != NULL)
    {
        profile->total_ms = monotonic_ms() - total_start;
    }

    return ret;
}
