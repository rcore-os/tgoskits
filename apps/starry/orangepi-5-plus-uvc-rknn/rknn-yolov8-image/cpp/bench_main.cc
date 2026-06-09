// Copyright (c) 2023 by Rockchip Electronics Co., Ltd. All Rights Reserved.
//
// Licensed under the Apache License, Version 2.0.

#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include <algorithm>
#include <vector>

#include "uvc_capture.h"
#include "yolov8.h"

struct Options {
    int device = 0;
    int width = 320;
    int height = 240;
    int fps = 30;
    int duration_sec = 60;
    int infer_every = 1;
    int report_interval_sec = 5;
    int min_confidence = 25;
    bool profile = false;
    bool profile_frames = false;
    bool set_core_mask = true;
    rknn_core_mask core_mask = RKNN_NPU_CORE_0_1_2;
    const char *core_mask_name = "0_1_2";
    const char *model_path = "model/yolov8.rknn";
    const char *label_path = "model/coco_80_labels_list.txt";
};

struct MemoryStats {
    long vm_size_kb = -1;
    long vm_rss_kb = -1;
    long vm_hwm_kb = -1;
    long mem_total_kb = -1;
    long mem_free_kb = -1;
    long mem_available_kb = -1;
};

struct ProfileSamples {
    std::vector<double> total_ms;
    std::vector<double> malloc_ms;
    std::vector<double> letterbox_ms;
    std::vector<double> inputs_set_ms;
    std::vector<double> run_ms;
    std::vector<double> outputs_get_ms;
    std::vector<double> rknn_perf_run_ms;
    std::vector<double> postprocess_ms;
    std::vector<double> outputs_release_ms;
    int perf_run_query_errors = 0;
};

static volatile sig_atomic_t g_running = 1;

static void signal_handler(int)
{
    g_running = 0;
}

static double monotonic_sec()
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1000000000.0;
}

static void print_usage(const char *argv0)
{
    printf("Usage: %s [OPTIONS]\n", argv0);
    printf("  --model <PATH>                 RKNN model [default: model/yolov8.rknn]\n");
    printf("  --label <PATH>                 label file [default: model/coco_80_labels_list.txt]\n");
    printf("  --device <INDEX>               UVC device index [default: 0]\n");
    printf("  --width <PIXELS>               frame width [default: 320]\n");
    printf("  --height <PIXELS>              frame height [default: 240]\n");
    printf("  --fps <FPS>                    camera FPS [default: 30]\n");
    printf("  --duration-sec <SECS>          run duration [default: 60]\n");
    printf("  --infer-every <N>              infer every Nth captured frame [default: 1]\n");
    printf("  --report-interval-sec <SECS>   progress interval, 0 disables [default: 5]\n");
    printf("  --min-confidence <PCT>         detection threshold percentage [default: 25]\n");
    printf("  --core-mask <MASK>             RKNN NPU core mask: none, auto, 0, 1, 2, 0_1, 0_1_2, all [default: 0_1_2]\n");
    printf("  --profile                      print final RKNN stage timing summary\n");
    printf("  --profile-frames               print one RKNN_PROFILE line per inference\n");
}

static bool parse_int_arg(const char *name, const char *value, int *out)
{
    if (value == NULL || value[0] == '\0') {
        printf("invalid value for %s\n", name);
        return false;
    }
    int parsed = 0;
    for (const char *p = value; *p != '\0'; ++p) {
        if (*p < '0' || *p > '9') {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
        parsed = parsed * 10 + (*p - '0');
        if (parsed > 1000000) {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
    }
    if (parsed <= 0) {
        printf("invalid value for %s: %s\n", name, value);
        return false;
    }
    *out = parsed;
    return true;
}

static bool parse_nonnegative_int_arg(const char *name, const char *value, int *out)
{
    if (value == NULL || value[0] == '\0') {
        printf("invalid value for %s\n", name);
        return false;
    }
    int parsed = 0;
    for (const char *p = value; *p != '\0'; ++p) {
        if (*p < '0' || *p > '9') {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
        parsed = parsed * 10 + (*p - '0');
        if (parsed > 1000000) {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
    }
    *out = parsed;
    return true;
}

static bool parse_core_mask_arg(const char *value, Options *options)
{
    if (strcmp(value, "none") == 0) {
        options->set_core_mask = false;
        options->core_mask_name = "none";
        return true;
    }
    options->set_core_mask = true;
    options->core_mask_name = value;
    if (strcmp(value, "auto") == 0) {
        options->core_mask = RKNN_NPU_CORE_AUTO;
    } else if (strcmp(value, "0") == 0) {
        options->core_mask = RKNN_NPU_CORE_0;
    } else if (strcmp(value, "1") == 0) {
        options->core_mask = RKNN_NPU_CORE_1;
    } else if (strcmp(value, "2") == 0) {
        options->core_mask = RKNN_NPU_CORE_2;
    } else if (strcmp(value, "0_1") == 0) {
        options->core_mask = RKNN_NPU_CORE_0_1;
    } else if (strcmp(value, "0_1_2") == 0) {
        options->core_mask = RKNN_NPU_CORE_0_1_2;
    } else if (strcmp(value, "all") == 0) {
        options->core_mask = RKNN_NPU_CORE_ALL;
    } else {
        printf("invalid value for --core-mask: %s\n", value);
        return false;
    }
    return true;
}

static bool parse_args(int argc, char **argv, Options *options)
{
    for (int i = 1; i < argc; ++i) {
        const char *arg = argv[i];
        const char *value = i + 1 < argc ? argv[i + 1] : NULL;
        if (strcmp(arg, "-h") == 0 || strcmp(arg, "--help") == 0) {
            print_usage(argv[0]);
            exit(0);
        }

        if (strcmp(arg, "--model") == 0 && value != NULL) {
            options->model_path = value;
            ++i;
        } else if (strcmp(arg, "--label") == 0 && value != NULL) {
            options->label_path = value;
            ++i;
        } else if (strcmp(arg, "--device") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->device)) return false;
            ++i;
        } else if (strcmp(arg, "--width") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->width)) return false;
            ++i;
        } else if (strcmp(arg, "--height") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->height)) return false;
            ++i;
        } else if (strcmp(arg, "--fps") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->fps)) return false;
            ++i;
        } else if (strcmp(arg, "--duration-sec") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->duration_sec)) return false;
            ++i;
        } else if (strcmp(arg, "--infer-every") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->infer_every)) return false;
            ++i;
        } else if (strcmp(arg, "--report-interval-sec") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->report_interval_sec)) return false;
            ++i;
        } else if (strcmp(arg, "--min-confidence") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->min_confidence)) return false;
            if (options->min_confidence < 1 || options->min_confidence > 99) {
                printf("invalid value for %s: %s\n", arg, value);
                return false;
            }
            ++i;
        } else if (strcmp(arg, "--core-mask") == 0 && value != NULL) {
            if (!parse_core_mask_arg(value, options)) return false;
            ++i;
        } else if (strcmp(arg, "--profile") == 0) {
            options->profile = true;
        } else if (strcmp(arg, "--profile-frames") == 0) {
            options->profile = true;
            options->profile_frames = true;
        } else {
            printf("unknown or incomplete argument: %s\n", arg);
            return false;
        }
    }
    return true;
}

static bool env_truthy(const char *name)
{
    const char *value = getenv(name);
    return value != NULL && value[0] != '\0' && strcmp(value, "0") != 0;
}

static void apply_env_options(Options *options)
{
    if (env_truthy("RKNN_BENCH_PROFILE")) {
        options->profile = true;
    }
    if (env_truthy("RKNN_BENCH_PROFILE_FRAMES")) {
        options->profile = true;
        options->profile_frames = true;
    }
}

static long parse_kb_line(const char *line, const char *key)
{
    size_t key_len = strlen(key);
    if (strncmp(line, key, key_len) != 0 || line[key_len] != ':') {
        return -1;
    }
    const char *p = line + key_len + 1;
    while (*p == ' ' || *p == '\t') {
        ++p;
    }
    char *end = NULL;
    long value = strtol(p, &end, 10);
    if (end == p || value < 0) {
        return -1;
    }
    return value;
}

static void read_status_memory(MemoryStats *stats)
{
    FILE *file = fopen("/proc/self/status", "r");
    if (file == NULL) {
        return;
    }
    char line[256];
    while (fgets(line, sizeof(line), file) != NULL) {
        long value = parse_kb_line(line, "VmSize");
        if (value >= 0) {
            stats->vm_size_kb = value;
            continue;
        }
        value = parse_kb_line(line, "VmRSS");
        if (value >= 0) {
            stats->vm_rss_kb = value;
            continue;
        }
        value = parse_kb_line(line, "VmHWM");
        if (value >= 0) {
            stats->vm_hwm_kb = value;
            continue;
        }
    }
    fclose(file);
}

static void read_meminfo_memory(MemoryStats *stats)
{
    FILE *file = fopen("/proc/meminfo", "r");
    if (file == NULL) {
        return;
    }
    char line[256];
    while (fgets(line, sizeof(line), file) != NULL) {
        long value = parse_kb_line(line, "MemTotal");
        if (value >= 0) {
            stats->mem_total_kb = value;
            continue;
        }
        value = parse_kb_line(line, "MemFree");
        if (value >= 0) {
            stats->mem_free_kb = value;
            continue;
        }
        value = parse_kb_line(line, "MemAvailable");
        if (value >= 0) {
            stats->mem_available_kb = value;
            continue;
        }
    }
    fclose(file);
}

static MemoryStats read_memory_stats()
{
    MemoryStats stats;
    read_status_memory(&stats);
    read_meminfo_memory(&stats);
    return stats;
}

static double percentile_ms(std::vector<double> samples, double percentile)
{
    if (samples.empty()) {
        return 0.0;
    }
    std::sort(samples.begin(), samples.end());
    double rank = percentile * (double)(samples.size() - 1);
    size_t low = (size_t)rank;
    size_t high = std::min(low + 1, samples.size() - 1);
    double fraction = rank - (double)low;
    return samples[low] * (1.0 - fraction) + samples[high] * fraction;
}

static double average_ms(const std::vector<double> &samples)
{
    if (samples.empty()) {
        return 0.0;
    }
    double total = 0.0;
    for (double sample : samples) {
        total += sample;
    }
    return total / (double)samples.size();
}

static void add_profile_sample(ProfileSamples *samples, const rknn_inference_profile_t *profile)
{
    samples->total_ms.push_back(profile->total_ms);
    samples->malloc_ms.push_back(profile->malloc_ms);
    samples->letterbox_ms.push_back(profile->letterbox_ms);
    samples->inputs_set_ms.push_back(profile->inputs_set_ms);
    samples->run_ms.push_back(profile->run_ms);
    samples->outputs_get_ms.push_back(profile->outputs_get_ms);
    samples->postprocess_ms.push_back(profile->postprocess_ms);
    samples->outputs_release_ms.push_back(profile->outputs_release_ms);
    if (profile->perf_run_query_ret == RKNN_SUCC && profile->rknn_perf_run_ms >= 0.0) {
        samples->rknn_perf_run_ms.push_back(profile->rknn_perf_run_ms);
    } else {
        samples->perf_run_query_errors++;
    }
}

static void print_profile_line(uint64_t frame_id, int ret, const rknn_inference_profile_t *profile, int detections)
{
    printf("RKNN_PROFILE frame=%llu ret=%d total_ms=%.2f malloc_ms=%.2f letterbox_ms=%.2f inputs_set_ms=%.2f run_ms=%.2f outputs_get_ms=%.2f rknn_perf_run_ms=%.2f perf_run_query_ret=%d postprocess_ms=%.2f outputs_release_ms=%.2f detections=%d\n",
           (unsigned long long)frame_id,
           ret,
           profile->total_ms,
           profile->malloc_ms,
           profile->letterbox_ms,
           profile->inputs_set_ms,
           profile->run_ms,
           profile->outputs_get_ms,
           profile->rknn_perf_run_ms,
           profile->perf_run_query_ret,
           profile->postprocess_ms,
           profile->outputs_release_ms,
           detections);
}

static void print_profile_summary(const ProfileSamples *samples)
{
    printf("UVC_RKNN_BENCH_PROFILE_RESULT profile_samples=%llu perf_run_query_errors=%d total_ms_avg=%.2f total_ms_p50=%.2f total_ms_p95=%.2f malloc_ms_avg=%.2f letterbox_ms_avg=%.2f letterbox_ms_p50=%.2f letterbox_ms_p95=%.2f inputs_set_ms_avg=%.2f run_ms_avg=%.2f run_ms_p50=%.2f run_ms_p95=%.2f outputs_get_ms_avg=%.2f outputs_get_ms_p50=%.2f outputs_get_ms_p95=%.2f rknn_perf_run_ms_avg=%.2f rknn_perf_run_ms_p50=%.2f rknn_perf_run_ms_p95=%.2f postprocess_ms_avg=%.2f postprocess_ms_p50=%.2f postprocess_ms_p95=%.2f outputs_release_ms_avg=%.2f\n",
           (unsigned long long)samples->total_ms.size(),
           samples->perf_run_query_errors,
           average_ms(samples->total_ms),
           percentile_ms(samples->total_ms, 0.50),
           percentile_ms(samples->total_ms, 0.95),
           average_ms(samples->malloc_ms),
           average_ms(samples->letterbox_ms),
           percentile_ms(samples->letterbox_ms, 0.50),
           percentile_ms(samples->letterbox_ms, 0.95),
           average_ms(samples->inputs_set_ms),
           average_ms(samples->run_ms),
           percentile_ms(samples->run_ms, 0.50),
           percentile_ms(samples->run_ms, 0.95),
           average_ms(samples->outputs_get_ms),
           percentile_ms(samples->outputs_get_ms, 0.50),
           percentile_ms(samples->outputs_get_ms, 0.95),
           average_ms(samples->rknn_perf_run_ms),
           percentile_ms(samples->rknn_perf_run_ms, 0.50),
           percentile_ms(samples->rknn_perf_run_ms, 0.95),
           average_ms(samples->postprocess_ms),
           percentile_ms(samples->postprocess_ms, 0.50),
           percentile_ms(samples->postprocess_ms, 0.95),
           average_ms(samples->outputs_release_ms));
}

int main(int argc, char **argv)
{
    Options options;
    if (!parse_args(argc, argv, &options)) {
        print_usage(argv[0]);
        return 2;
    }
    apply_env_options(&options);

    signal(SIGINT, signal_handler);
    signal(SIGTERM, signal_handler);
    signal(SIGPIPE, SIG_IGN);

    printf("YOLOv8 UVC RKNN Benchmark\n");
    printf("=========================\n");
    printf("model=%s label=%s device=%d size=%dx%d fps=%d duration=%d infer_every=%d report_interval=%d min_confidence=%d core_mask=%s profile=%d profile_frames=%d\n",
           options.model_path,
           options.label_path,
           options.device,
           options.width,
           options.height,
           options.fps,
           options.duration_sec,
           options.infer_every,
           options.report_interval_sec,
           options.min_confidence,
           options.core_mask_name,
           options.profile ? 1 : 0,
           options.profile_frames ? 1 : 0);

    int ret = init_post_process(options.label_path);
    if (ret != 0) {
        printf("init_post_process fail! ret=%d label_path=%s\n", ret, options.label_path);
        return 1;
    }

    rknn_app_context_t app_ctx;
    memset(&app_ctx, 0, sizeof(app_ctx));
    ret = init_yolov8_model(options.model_path, &app_ctx);
    if (ret != 0) {
        printf("init_yolov8_model fail! ret=%d model_path=%s\n", ret, options.model_path);
        deinit_post_process();
        return 1;
    }
    printf("bench-rknn: init_yolov8_model success width=%d height=%d channel=%d\n",
           app_ctx.model_width,
           app_ctx.model_height,
           app_ctx.model_channel);
    if (options.set_core_mask) {
        ret = rknn_set_core_mask(app_ctx.rknn_ctx, options.core_mask);
        printf("bench-rknn: set_core_mask=%s ret=%d\n", options.core_mask_name, ret);
        if (ret != RKNN_SUCC) {
            release_yolov8_model(&app_ctx);
            deinit_post_process();
            return 1;
        }
    } else {
        printf("bench-rknn: set_core_mask=none\n");
    }

    UvcCaptureSession capture;
    UvcCaptureOptions capture_options;
    capture_options.device = options.device;
    capture_options.width = options.width;
    capture_options.height = options.height;
    capture_options.fps = options.fps;
    capture_options.log_prefix = "bench-rknn";
    if (!start_uvc_capture(&capture, &capture_options)) {
        release_yolov8_model(&app_ctx);
        deinit_post_process();
        return 1;
    }
    SharedState *state = &capture.state;

    const double start = monotonic_sec();
    double last_report = start;
    uint64_t last_report_captured = 0;
    uint64_t last_inferred_frame = 0;
    const uint64_t infer_interval = (uint64_t)options.infer_every;
    int inferences = 0;
    int decode_errors = 0;
    int inference_errors = 0;
    uint64_t detections = 0;
    std::vector<double> decode_ms;
    std::vector<double> infer_ms;
    ProfileSamples profile_samples;

    while (g_running && monotonic_sec() - start < options.duration_sec) {
        LatestFrame frame;
        {
            std::lock_guard<std::mutex> guard(state->mutex);
            if (state->latest.id != 0 && state->latest.id != last_inferred_frame &&
                state->latest.id >= last_inferred_frame + infer_interval) {
                frame = state->latest;
            }
        }

        if (frame.id == 0) {
            usleep(1000);
        } else {
            image_buffer_t image;
            memset(&image, 0, sizeof(image));
            double decode_start = monotonic_sec();
            if (frame_to_image(frame, &image) != 0) {
                decode_errors++;
                last_inferred_frame = frame.id;
                continue;
            }
            double infer_start = monotonic_sec();

            object_detect_result_list results;
            memset(&results, 0, sizeof(results));
            rknn_inference_profile_t profile;
            memset(&profile, 0, sizeof(profile));
            if (options.profile) {
                ret = inference_yolov8_model_with_thresholds_profile(
                    &app_ctx,
                    &image,
                    &results,
                    (float)options.min_confidence / 100.0f,
                    NMS_THRESH,
                    &profile);
            } else {
                ret = inference_yolov8_model_with_thresholds(
                    &app_ctx,
                    &image,
                    &results,
                    (float)options.min_confidence / 100.0f,
                    NMS_THRESH);
            }
            double infer_end = monotonic_sec();
            last_inferred_frame = frame.id;
            decode_ms.push_back((infer_start - decode_start) * 1000.0);
            infer_ms.push_back((infer_end - infer_start) * 1000.0);
            if (options.profile_frames) {
                print_profile_line(frame.id, ret, &profile, ret == 0 ? results.count : 0);
            }
            if (ret != 0) {
                printf("inference_yolov8_model fail! ret=%d frame=%llu\n", ret, (unsigned long long)frame.id);
                inference_errors++;
            } else {
                inferences++;
                detections += (uint64_t)results.count;
                if (options.profile) {
                    add_profile_sample(&profile_samples, &profile);
                }
            }
            free(image.virt_addr);
            sched_yield();
        }

        double now = monotonic_sec();
        if (options.report_interval_sec > 0 && now - last_report >= options.report_interval_sec) {
            UvcCaptureCounters counters = capture_counters(state);
            double interval = now - last_report;
            uint64_t delta = counters.captured - last_report_captured;
            printf("bench-rknn: capture_fps=%.2f captured=%llu inferred=%d infer_fps=%.2f dropped_latest=%llu mib_s=%.2f elapsed=%.1f\n",
                   (double)delta / std::max(interval, 0.001),
                   (unsigned long long)counters.captured,
                   inferences,
                   (double)inferences / std::max(now - start, 0.001),
                   (unsigned long long)counters.dropped,
                   (double)counters.bytes / std::max(now - start, 0.001) / 1024.0 / 1024.0,
                   now - start);
            fflush(stdout);
            last_report = now;
            last_report_captured = counters.captured;
        }
    }

    stop_uvc_capture(&capture);

    double elapsed = monotonic_sec() - start;
    UvcCaptureCounters counters = capture_counters(state);
    MemoryStats memory = read_memory_stats();
    int release_ret = release_yolov8_model(&app_ctx);
    deinit_post_process();

    double safe_elapsed = std::max(elapsed, 0.001);
    printf("UVC_RKNN_BENCH_RESULT duration_sec=%.1f captured=%llu capture_fps=%.2f inferences=%d infer_fps=%.2f bytes=%llu throughput_mib_s=%.2f dropped_latest=%llu decode_errors=%d inference_errors=%d decode_ms_avg=%.2f decode_ms_p50=%.2f decode_ms_p95=%.2f infer_ms_avg=%.2f infer_ms_p50=%.2f infer_ms_p95=%.2f detections=%llu vm_size_kb=%ld vm_rss_kb=%ld vm_hwm_kb=%ld mem_total_kb=%ld mem_free_kb=%ld mem_available_kb=%ld\n",
           elapsed,
           (unsigned long long)counters.captured,
           (double)counters.captured / safe_elapsed,
           inferences,
           (double)inferences / safe_elapsed,
           (unsigned long long)counters.bytes,
           (double)counters.bytes / safe_elapsed / 1024.0 / 1024.0,
           (unsigned long long)counters.dropped,
           decode_errors,
           inference_errors,
           average_ms(decode_ms),
           percentile_ms(decode_ms, 0.50),
           percentile_ms(decode_ms, 0.95),
           average_ms(infer_ms),
           percentile_ms(infer_ms, 0.50),
           percentile_ms(infer_ms, 0.95),
           (unsigned long long)detections,
           memory.vm_size_kb,
           memory.vm_rss_kb,
           memory.vm_hwm_kb,
           memory.mem_total_kb,
           memory.mem_free_kb,
           memory.mem_available_kb);
    if (options.profile) {
        print_profile_summary(&profile_samples);
    }

    if (release_ret == 0 && counters.captured > 0 && inferences > 0 && inference_errors == 0) {
        printf("UVC_RKNN_BENCH_DONE\n");
        return 0;
    }
    return 1;
}
