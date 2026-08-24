#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "rknn_api.h"

#define IVC_RKNN_INPUTS 4U
#define IVC_RKNN_VERSION_CAPACITY 256U

enum ivc_rknn_stage {
    IVC_RKNN_STAGE_OPEN_MODEL = 1,
    IVC_RKNN_STAGE_READ_MODEL = 2,
    IVC_RKNN_STAGE_ALLOCATE_CONTEXT = 3,
    IVC_RKNN_STAGE_INITIALIZE_CONTEXT = 4,
    IVC_RKNN_STAGE_SET_CORE_MASK = 5,
    IVC_RKNN_STAGE_QUERY_IO_COUNTS = 6,
    IVC_RKNN_STAGE_QUERY_INPUT_ATTRIBUTE = 7,
    IVC_RKNN_STAGE_QUERY_OUTPUT_ATTRIBUTE = 8,
    IVC_RKNN_STAGE_VALIDATE_TENSOR_CONTRACT = 9,
    IVC_RKNN_STAGE_SET_INPUT = 10,
    IVC_RKNN_STAGE_RUN = 11,
    IVC_RKNN_STAGE_GET_OUTPUT = 12,
    IVC_RKNN_STAGE_VALIDATE_OUTPUT = 13,
    IVC_RKNN_STAGE_QUERY_PERFORMANCE = 14,
    IVC_RKNN_STAGE_RELEASE_OUTPUT = 15,
    IVC_RKNN_STAGE_DESTROY_CONTEXT = 16,
    IVC_RKNN_STAGE_MONOTONIC_CLOCK = 17,
    IVC_RKNN_STAGE_QUERY_VERSION = 18,
};

struct ivc_rknn_status {
    int32_t stage;
    int32_t vendor_status;
};

struct ivc_rknn_info {
    char api_version[IVC_RKNN_VERSION_CAPACITY];
    char driver_version[IVC_RKNN_VERSION_CAPACITY];
    uint64_t init_us;
};

struct ivc_rknn_inference {
    float output;
    uint64_t wall_ns;
    int64_t device_us;
};

struct ivc_rknn_context {
    rknn_context handle;
    void *model;
    uint32_t model_size;
    rknn_tensor_format input_format;
};

static int bridge_fail(struct ivc_rknn_status *status, int32_t stage,
                       int32_t vendor_status) {
    if (status != NULL) {
        status->stage = stage;
        status->vendor_status = vendor_status;
    }
    return -1;
}

static void bridge_status_clear(struct ivc_rknn_status *status) {
    if (status != NULL) {
        status->stage = 0;
        status->vendor_status = 0;
    }
}

static int monotonic_ns(uint64_t *result, struct ivc_rknn_status *status) {
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != 0 || value.tv_sec < 0 ||
        value.tv_nsec < 0 || value.tv_nsec >= 1000000000L) {
        return bridge_fail(status, IVC_RKNN_STAGE_MONOTONIC_CLOCK, 0);
    }
    *result = (uint64_t)value.tv_sec * UINT64_C(1000000000) +
              (uint64_t)value.tv_nsec;
    return 0;
}

static int tensor_name_is(const char *actual, const char *expected,
                          size_t capacity) {
    return strncmp(actual, expected, capacity) == 0;
}

static void release_context(struct ivc_rknn_context *context) {
    if (context == NULL) {
        return;
    }
    if (context->handle != 0) {
        rknn_destroy(context->handle);
        context->handle = 0;
    }
    free(context->model);
    context->model = NULL;
    free(context);
}

int ivc_rknn_create(const char *model_path, uint32_t core_mask,
                    void **raw_context, struct ivc_rknn_info *info,
                    struct ivc_rknn_status *status) {
    FILE *model_file = NULL;
    long model_size = 0;
    struct ivc_rknn_context *context = NULL;
    rknn_input_output_num counts;
    rknn_tensor_attr input_attr;
    rknn_tensor_attr output_attr;
    rknn_sdk_version versions;
    uint64_t init_started_ns = 0;
    uint64_t init_finished_ns = 0;
    int vendor_status = 0;
    size_t bytes_read = 0;
    int close_status = 0;

    bridge_status_clear(status);
    if (model_path == NULL || raw_context == NULL || info == NULL) {
        return bridge_fail(status, IVC_RKNN_STAGE_ALLOCATE_CONTEXT, 0);
    }
    *raw_context = NULL;
    memset(info, 0, sizeof(*info));

    model_file = fopen(model_path, "rb");
    if (model_file == NULL) {
        return bridge_fail(status, IVC_RKNN_STAGE_OPEN_MODEL, 0);
    }
    if (fseek(model_file, 0, SEEK_END) != 0 ||
        (model_size = ftell(model_file)) <= 0 ||
        (uint64_t)model_size > UINT32_MAX ||
        fseek(model_file, 0, SEEK_SET) != 0) {
        fclose(model_file);
        return bridge_fail(status, IVC_RKNN_STAGE_READ_MODEL, 0);
    }

    context = calloc(1, sizeof(*context));
    if (context == NULL) {
        fclose(model_file);
        return bridge_fail(status, IVC_RKNN_STAGE_ALLOCATE_CONTEXT, 0);
    }
    context->model_size = (uint32_t)model_size;
    context->model = malloc(context->model_size);
    if (context->model == NULL) {
        fclose(model_file);
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_ALLOCATE_CONTEXT, 0);
    }
    bytes_read = fread(context->model, 1, context->model_size, model_file);
    close_status = fclose(model_file);
    if (bytes_read != context->model_size || close_status != 0) {
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_READ_MODEL, 0);
    }

    if (monotonic_ns(&init_started_ns, status) != 0) {
        release_context(context);
        return -1;
    }
    vendor_status = rknn_init(&context->handle, context->model,
                              context->model_size,
                              RKNN_FLAG_COLLECT_PERF_MASK, NULL);
    if (vendor_status != RKNN_SUCC) {
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_INITIALIZE_CONTEXT,
                           vendor_status);
    }
    vendor_status =
        rknn_set_core_mask(context->handle, (rknn_core_mask)core_mask);
    if (vendor_status != RKNN_SUCC) {
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_SET_CORE_MASK,
                           vendor_status);
    }
    if (monotonic_ns(&init_finished_ns, status) != 0 ||
        init_finished_ns <= init_started_ns) {
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_MONOTONIC_CLOCK, 0);
    }

    memset(&counts, 0, sizeof(counts));
    vendor_status = rknn_query(context->handle, RKNN_QUERY_IN_OUT_NUM, &counts,
                               sizeof(counts));
    if (vendor_status != RKNN_SUCC) {
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_QUERY_IO_COUNTS,
                           vendor_status);
    }
    if (counts.n_input != 1 || counts.n_output != 1) {
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_VALIDATE_TENSOR_CONTRACT, 0);
    }

    memset(&input_attr, 0, sizeof(input_attr));
    input_attr.index = 0;
    vendor_status = rknn_query(context->handle, RKNN_QUERY_INPUT_ATTR,
                               &input_attr, sizeof(input_attr));
    if (vendor_status != RKNN_SUCC) {
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_QUERY_INPUT_ATTRIBUTE,
                           vendor_status);
    }
    memset(&output_attr, 0, sizeof(output_attr));
    output_attr.index = 0;
    vendor_status = rknn_query(context->handle, RKNN_QUERY_OUTPUT_ATTR,
                               &output_attr, sizeof(output_attr));
    if (vendor_status != RKNN_SUCC) {
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_QUERY_OUTPUT_ATTRIBUTE,
                           vendor_status);
    }
    if (!tensor_name_is(input_attr.name, "normalized_observation",
                        sizeof(input_attr.name)) ||
        input_attr.n_elems != IVC_RKNN_INPUTS ||
        input_attr.type != RKNN_TENSOR_FLOAT16 ||
        !tensor_name_is(output_attr.name, "control_fraction",
                        sizeof(output_attr.name)) ||
        output_attr.n_elems != 1 ||
        output_attr.type != RKNN_TENSOR_FLOAT16) {
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_VALIDATE_TENSOR_CONTRACT, 0);
    }
    context->input_format = input_attr.fmt;

    memset(&versions, 0, sizeof(versions));
    vendor_status = rknn_query(context->handle, RKNN_QUERY_SDK_VERSION,
                               &versions, sizeof(versions));
    if (vendor_status != RKNN_SUCC) {
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_QUERY_VERSION, vendor_status);
    }
    memcpy(info->api_version, versions.api_version,
           sizeof(info->api_version));
    memcpy(info->driver_version, versions.drv_version,
           sizeof(info->driver_version));
    info->api_version[sizeof(info->api_version) - 1] = '\0';
    info->driver_version[sizeof(info->driver_version) - 1] = '\0';
    info->init_us = (init_finished_ns - init_started_ns) / UINT64_C(1000);
    if (info->init_us == 0) {
        release_context(context);
        return bridge_fail(status, IVC_RKNN_STAGE_MONOTONIC_CLOCK, 0);
    }
    *raw_context = context;
    return 0;
}

int ivc_rknn_infer(void *raw_context, const float *inputs,
                   struct ivc_rknn_inference *inference,
                   struct ivc_rknn_status *status) {
    struct ivc_rknn_context *context = raw_context;
    rknn_input input;
    rknn_output output;
    rknn_perf_run performance;
    uint64_t started_ns = 0;
    uint64_t finished_ns = 0;
    int vendor_status = 0;
    int release_status = 0;
    uint32_t index = 0;

    bridge_status_clear(status);
    if (context == NULL || inputs == NULL || inference == NULL) {
        return bridge_fail(status, IVC_RKNN_STAGE_VALIDATE_OUTPUT, 0);
    }
    memset(inference, 0, sizeof(*inference));
    for (index = 0; index < IVC_RKNN_INPUTS; ++index) {
        if (!isfinite(inputs[index])) {
            return bridge_fail(status,
                               IVC_RKNN_STAGE_VALIDATE_TENSOR_CONTRACT, 0);
        }
    }
    if (monotonic_ns(&started_ns, status) != 0) {
        return -1;
    }

    memset(&input, 0, sizeof(input));
    input.index = 0;
    input.buf = (void *)inputs;
    input.size = sizeof(float) * IVC_RKNN_INPUTS;
    input.pass_through = 0;
    input.type = RKNN_TENSOR_FLOAT32;
    input.fmt = context->input_format;
    vendor_status = rknn_inputs_set(context->handle, 1, &input);
    if (vendor_status != RKNN_SUCC) {
        return bridge_fail(status, IVC_RKNN_STAGE_SET_INPUT, vendor_status);
    }
    vendor_status = rknn_run(context->handle, NULL);
    if (vendor_status != RKNN_SUCC) {
        return bridge_fail(status, IVC_RKNN_STAGE_RUN, vendor_status);
    }

    memset(&output, 0, sizeof(output));
    output.index = 0;
    output.want_float = 1;
    output.is_prealloc = 0;
    vendor_status = rknn_outputs_get(context->handle, 1, &output, NULL);
    if (vendor_status != RKNN_SUCC) {
        return bridge_fail(status, IVC_RKNN_STAGE_GET_OUTPUT, vendor_status);
    }
    if (output.buf == NULL || output.size < sizeof(float)) {
        rknn_outputs_release(context->handle, 1, &output);
        return bridge_fail(status, IVC_RKNN_STAGE_VALIDATE_OUTPUT, 0);
    }
    memcpy(&inference->output, output.buf, sizeof(inference->output));
    if (!isfinite(inference->output)) {
        rknn_outputs_release(context->handle, 1, &output);
        return bridge_fail(status, IVC_RKNN_STAGE_VALIDATE_OUTPUT, 0);
    }
    if (monotonic_ns(&finished_ns, status) != 0 ||
        finished_ns <= started_ns) {
        rknn_outputs_release(context->handle, 1, &output);
        return bridge_fail(status, IVC_RKNN_STAGE_MONOTONIC_CLOCK, 0);
    }

    memset(&performance, 0, sizeof(performance));
    vendor_status = rknn_query(context->handle, RKNN_QUERY_PERF_RUN,
                               &performance, sizeof(performance));
    release_status = rknn_outputs_release(context->handle, 1, &output);
    if (vendor_status != RKNN_SUCC) {
        return bridge_fail(status, IVC_RKNN_STAGE_QUERY_PERFORMANCE,
                           vendor_status);
    }
    if (release_status != RKNN_SUCC) {
        return bridge_fail(status, IVC_RKNN_STAGE_RELEASE_OUTPUT,
                           release_status);
    }
    if (performance.run_duration <= 0) {
        return bridge_fail(status, IVC_RKNN_STAGE_QUERY_PERFORMANCE, 0);
    }
    inference->wall_ns = finished_ns - started_ns;
    inference->device_us = performance.run_duration;
    return 0;
}

int ivc_rknn_destroy(void *raw_context, struct ivc_rknn_status *status) {
    struct ivc_rknn_context *context = raw_context;
    int vendor_status = 0;

    bridge_status_clear(status);
    if (context == NULL) {
        return bridge_fail(status, IVC_RKNN_STAGE_DESTROY_CONTEXT, 0);
    }
    if (context->handle != 0) {
        vendor_status = rknn_destroy(context->handle);
        context->handle = 0;
    }
    free(context->model);
    context->model = NULL;
    free(context);
    if (vendor_status != RKNN_SUCC) {
        return bridge_fail(status, IVC_RKNN_STAGE_DESTROY_CONTEXT,
                           vendor_status);
    }
    return 0;
}
