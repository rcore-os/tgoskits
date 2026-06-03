#include "k230_sdk_compat.h"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <iterator>
#include <limits>
#include <string>
#include <vector>

#include <jpeglib.h>
#include <nncase/runtime/interpreter.h>
#include <nncase/runtime/runtime_op_utility.h>
#include <sys/mman.h>

using namespace nncase;
using namespace nncase::runtime;
using namespace nncase::runtime::detail;

namespace {

struct Image {
    int width = 0;
    int height = 0;
    std::vector<uint8_t> rgb;
};

struct Detection {
    float x1 = 0;
    float y1 = 0;
    float x2 = 0;
    float y2 = 0;
    float score = 0;
    int class_id = 0;
};

struct OutputStats {
    float min = std::numeric_limits<float>::infinity();
    float max = -std::numeric_limits<float>::infinity();
    float mean = 0.0f;
    size_t finite = 0;
    size_t nan = 0;
};

struct ScoreCandidate {
    size_t row = 0;
    int class_id = -1;
    float score = -std::numeric_limits<float>::infinity();
    float x = 0;
    float y = 0;
    float w = 0;
    float h = 0;
};

constexpr float kScoreThreshold = 0.15f;
constexpr float kNmsThreshold = 0.20f;
constexpr uint64_t kYoloDirectBboxPaddr = 0x1059a900ull;
constexpr uint64_t kYoloDirectClassPaddr = 0x105b0e20ull;
constexpr size_t kYoloRows = 2100;
constexpr size_t kYoloClasses = 80;

constexpr const char *kCocoClasses[] = {
    "person",        "bicycle",      "car",          "motorcycle",
    "airplane",      "bus",          "train",        "truck",
    "boat",          "traffic light", "fire hydrant", "stop sign",
    "parking meter", "bench",        "bird",         "cat",
    "dog",           "horse",        "sheep",        "cow",
    "elephant",      "bear",         "zebra",        "giraffe",
    "backpack",      "umbrella",     "handbag",      "tie",
    "suitcase",      "frisbee",      "skis",         "snowboard",
    "sports ball",   "kite",         "baseball bat", "baseball glove",
    "skateboard",    "surfboard",    "tennis racket", "bottle",
    "wine glass",    "cup",          "fork",         "knife",
    "spoon",         "bowl",         "banana",       "apple",
    "sandwich",      "orange",       "broccoli",     "carrot",
    "hot dog",       "pizza",        "donut",        "cake",
    "chair",         "couch",        "potted plant", "bed",
    "dining table",  "toilet",       "tv",           "laptop",
    "mouse",         "remote",       "keyboard",     "cell phone",
    "microwave",     "oven",         "toaster",      "sink",
    "refrigerator",  "book",         "clock",        "vase",
    "scissors",      "teddy bear",   "hair drier",   "toothbrush",
};

static void print_shape(const dims_t &shape) {
    std::printf("[");
    for (size_t i = 0; i < shape.size(); ++i) {
        if (i) {
            std::printf(",");
        }
        std::printf("%zu", shape[i]);
    }
    std::printf("]");
}

static std::vector<uint8_t> read_file(const char *path) {
    FILE *fp = std::fopen(path, "rb");
    if (!fp) {
        return {};
    }
    if (std::fseek(fp, 0, SEEK_END) != 0) {
        std::fclose(fp);
        return {};
    }
    long len = std::ftell(fp);
    if (len <= 0) {
        std::fclose(fp);
        return {};
    }
    std::rewind(fp);
    std::vector<uint8_t> data(static_cast<size_t>(len));
    size_t got = std::fread(data.data(), 1, data.size(), fp);
    std::fclose(fp);
    if (got != data.size()) {
        return {};
    }
    return data;
}

static bool decode_jpeg(const char *path, Image &image) {
    FILE *fp = std::fopen(path, "rb");
    if (!fp) {
        return false;
    }

    jpeg_decompress_struct cinfo;
    jpeg_error_mgr jerr;
    cinfo.err = jpeg_std_error(&jerr);
    jpeg_create_decompress(&cinfo);
    jpeg_stdio_src(&cinfo, fp);
    jpeg_read_header(&cinfo, TRUE);
    cinfo.out_color_space = JCS_RGB;
    jpeg_start_decompress(&cinfo);

    image.width = static_cast<int>(cinfo.output_width);
    image.height = static_cast<int>(cinfo.output_height);
    image.rgb.assign(static_cast<size_t>(image.width) * image.height * 3, 0);

    std::vector<uint8_t> row(static_cast<size_t>(image.width) * 3);
    while (cinfo.output_scanline < cinfo.output_height) {
        JSAMPROW row_pointer = row.data();
        jpeg_read_scanlines(&cinfo, &row_pointer, 1);
        size_t y = static_cast<size_t>(cinfo.output_scanline - 1);
        std::memcpy(image.rgb.data() + y * row.size(), row.data(), row.size());
    }

    jpeg_finish_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    std::fclose(fp);
    return image.width > 0 && image.height > 0 && !image.rgb.empty();
}

static uint64_t fnv1a64(const uint8_t *data, size_t bytes) {
    uint64_t hash = 1469598103934665603ULL;
    for (size_t i = 0; i < bytes; ++i) {
        hash ^= data[i];
        hash *= 1099511628211ULL;
    }
    return hash;
}

static OutputStats summarize_floats(const float *data, size_t count) {
    OutputStats stats;
    double sum = 0.0;
    for (size_t i = 0; i < count; ++i) {
        float v = data[i];
        if (!std::isfinite(v)) {
            stats.nan++;
            continue;
        }
        stats.min = std::min(stats.min, v);
        stats.max = std::max(stats.max, v);
        sum += v;
        stats.finite++;
    }
    if (stats.finite != 0) {
        stats.mean = static_cast<float>(sum / static_cast<double>(stats.finite));
    } else {
        stats.min = 0.0f;
        stats.max = 0.0f;
    }
    return stats;
}

static void print_runtime_paddr(const char *prefix, size_t index,
                                const void *data, size_t bytes) {
    k_sys_virmem_info info = {};
    if (kd_mpi_sys_get_virmem_info(data, &info) == 0) {
        std::printf(
            "%s[%zu] paddr=0x%016llx bytes=%zu cached=%d\n", prefix, index,
            static_cast<unsigned long long>(info.phy_addr), bytes, info.cached);
    } else {
        std::printf("%s[%zu] paddr=unknown bytes=%zu\n", prefix, index, bytes);
    }
}

static bool direct_ptr(uint8_t *base, uint64_t paddr, size_t bytes,
                       const uint8_t **ptr) {
    if (paddr < KPU_RUNTIME_DIRECT_IO_PADDR ||
        paddr - KPU_RUNTIME_DIRECT_IO_PADDR > KPU_RUNTIME_DIRECT_IO_SIZE ||
        bytes > KPU_RUNTIME_DIRECT_IO_SIZE -
                    (paddr - KPU_RUNTIME_DIRECT_IO_PADDR)) {
        return false;
    }
    *ptr = base + (paddr - KPU_RUNTIME_DIRECT_IO_PADDR);
    return true;
}

static void print_direct_float_range(const char *name, uint8_t *base,
                                     uint64_t paddr, size_t bytes) {
    const uint8_t *ptr = nullptr;
    if (!direct_ptr(base, paddr, bytes, &ptr)) {
        std::printf(
            "YOLOV8N_DEMO: direct[%s] unavailable paddr=0x%016llx bytes=%zu\n",
            name, static_cast<unsigned long long>(paddr), bytes);
        return;
    }
    uint64_t hash = fnv1a64(ptr, bytes);
    auto stats =
        summarize_floats(reinterpret_cast<const float *>(ptr), bytes / sizeof(float));
    std::printf(
        "YOLOV8N_DEMO: direct[%s] paddr=0x%016llx bytes=%zu fnv1a64=0x%016llx\n",
        name, static_cast<unsigned long long>(paddr), bytes,
        static_cast<unsigned long long>(hash));
    std::printf(
        "YOLOV8N_DEMO: direct[%s] stats finite=%zu nan=%zu min=%.6f max=%.6f mean=%.6f\n",
        name, stats.finite, stats.nan, stats.min, stats.max, stats.mean);
}

static void inspect_direct_yolo_output() {
    void *map =
        kd_mpi_sys_mmap_cached(KPU_RUNTIME_DIRECT_IO_PADDR,
                               KPU_RUNTIME_DIRECT_IO_SIZE);
    if (map == MAP_FAILED) {
        std::printf("YOLOV8N_DEMO: direct inspect unavailable\n");
        return;
    }

    auto *base = static_cast<uint8_t *>(map);
    const size_t bbox_bytes = 4 * kYoloRows * sizeof(float);
    const size_t class_bytes = kYoloClasses * kYoloRows * sizeof(float);
    print_direct_float_range("yolo_bbox", base, kYoloDirectBboxPaddr,
                             bbox_bytes);
    print_direct_float_range("yolo_class", base, kYoloDirectClassPaddr,
                             class_bytes);

    const uint8_t *bbox_bytes_ptr = nullptr;
    const uint8_t *class_bytes_ptr = nullptr;
    if (direct_ptr(base, kYoloDirectBboxPaddr, bbox_bytes, &bbox_bytes_ptr) &&
        direct_ptr(base, kYoloDirectClassPaddr, class_bytes, &class_bytes_ptr)) {
        const auto *bbox = reinterpret_cast<const float *>(bbox_bytes_ptr);
        const auto *classes = reinterpret_cast<const float *>(class_bytes_ptr);
        std::vector<ScoreCandidate> top_scores;
        auto remember_score = [&](const ScoreCandidate &candidate) {
            auto insert_at = top_scores.begin();
            while (insert_at != top_scores.end() &&
                   insert_at->score >= candidate.score) {
                ++insert_at;
            }
            top_scores.insert(insert_at, candidate);
            if (top_scores.size() > 5) {
                top_scores.pop_back();
            }
        };
        for (size_t r = 0; r < kYoloRows; ++r) {
            for (size_t c = 0; c < kYoloClasses; ++c) {
                float score = classes[c * kYoloRows + r];
                if (!std::isfinite(score)) {
                    continue;
                }
                remember_score({r, static_cast<int>(c), score,
                                bbox[0 * kYoloRows + r],
                                bbox[1 * kYoloRows + r],
                                bbox[2 * kYoloRows + r],
                                bbox[3 * kYoloRows + r]});
            }
        }
        for (size_t i = 0; i < top_scores.size(); ++i) {
            const auto &candidate = top_scores[i];
            const char *name =
                static_cast<size_t>(candidate.class_id) < std::size(kCocoClasses)
                    ? kCocoClasses[candidate.class_id]
                    : "unknown";
            std::printf(
                "YOLOV8N_DEMO: direct_score_top[%zu] row=%zu class=%s score=%.6f box_raw=(%.3f,%.3f,%.3f,%.3f)\n",
                i, candidate.row, name, candidate.score, candidate.x,
                candidate.y, candidate.w, candidate.h);
        }
    }
    kd_mpi_sys_munmap(map, KPU_RUNTIME_DIRECT_IO_SIZE);
}

static bool mirror_tensor_to_direct(runtime_tensor &tensor, uint64_t paddr,
                                    const char *name) {
    auto mapped = tensor.impl()
                      ->to_host()
                      .unwrap()
                      ->buffer()
                      .as_host()
                      .unwrap()
                      .map(map_access_::map_read)
                      .unwrap()
                      .buffer();
    void *direct = kd_mpi_sys_mmap_cached(paddr, mapped.size_bytes());
    if (direct == MAP_FAILED) {
        std::printf(
            "YOLOV8N_DEMO_FAIL: cannot map direct[%s] paddr=0x%016llx bytes=%zu\n",
            name, static_cast<unsigned long long>(paddr), mapped.size_bytes());
        return false;
    }
    const auto *src = reinterpret_cast<const uint8_t *>(mapped.data());
    std::memcpy(direct, src, mapped.size_bytes());
    uint64_t hash = fnv1a64(src, mapped.size_bytes());
    print_runtime_paddr("YOLOV8N_DEMO: input", 0, mapped.data(),
                        mapped.size_bytes());
    std::printf(
        "YOLOV8N_DEMO: direct[%s] copied paddr=0x%016llx bytes=%zu fnv1a64=0x%016llx\n",
        name, static_cast<unsigned long long>(paddr), mapped.size_bytes(),
        static_cast<unsigned long long>(hash));
    kd_mpi_sys_munmap(direct, mapped.size_bytes());
    return true;
}

static uint8_t sample_bilinear(const Image &image, float x, float y, int channel) {
    x = std::max(0.0f, std::min(x, static_cast<float>(image.width - 1)));
    y = std::max(0.0f, std::min(y, static_cast<float>(image.height - 1)));
    int x0 = static_cast<int>(std::floor(x));
    int y0 = static_cast<int>(std::floor(y));
    int x1 = std::min(x0 + 1, image.width - 1);
    int y1 = std::min(y0 + 1, image.height - 1);
    float fx = x - x0;
    float fy = y - y0;
    auto at = [&](int px, int py) -> float {
        size_t index = (static_cast<size_t>(py) * image.width + px) * 3 + channel;
        return static_cast<float>(image.rgb[index]);
    };
    float top = at(x0, y0) * (1.0f - fx) + at(x1, y0) * fx;
    float bottom = at(x0, y1) * (1.0f - fx) + at(x1, y1) * fx;
    return static_cast<uint8_t>(std::lround(top * (1.0f - fy) + bottom * fy));
}

static bool fill_input_from_image(runtime_tensor &tensor, const dims_t &shape,
                                  typecode_t datatype, const Image &image) {
    if (shape.size() != 4) {
        return false;
    }
    bool nchw = shape[1] == 3;
    bool nhwc = shape[3] == 3;
    if (!nchw && !nhwc) {
        return false;
    }

    size_t channels = 3;
    size_t input_h = nchw ? shape[2] : shape[1];
    size_t input_w = nchw ? shape[3] : shape[2];
    auto mapped = tensor.impl()
                      ->to_host()
                      .unwrap()
                      ->buffer()
                      .as_host()
                      .unwrap()
                      .map(map_access_::map_write)
                      .unwrap()
                      .buffer();

    float scale_x = static_cast<float>(image.width) / static_cast<float>(input_w);
    float scale_y = static_cast<float>(image.height) / static_cast<float>(input_h);

    if (datatype == dt_uint8 || datatype == dt_int8) {
        auto *dst = reinterpret_cast<uint8_t *>(mapped.data());
        for (size_t y = 0; y < input_h; ++y) {
            for (size_t x = 0; x < input_w; ++x) {
                float src_x = (static_cast<float>(x) + 0.5f) * scale_x - 0.5f;
                float src_y = (static_cast<float>(y) + 0.5f) * scale_y - 0.5f;
                for (size_t c = 0; c < channels; ++c) {
                    uint8_t v = sample_bilinear(image, src_x, src_y, static_cast<int>(c));
                    size_t offset = nchw ? c * input_h * input_w + y * input_w + x
                                         : (y * input_w + x) * channels + c;
                    dst[offset] = v;
                }
            }
        }
    } else if (datatype == dt_float32) {
        auto *dst = reinterpret_cast<float *>(mapped.data());
        for (size_t y = 0; y < input_h; ++y) {
            for (size_t x = 0; x < input_w; ++x) {
                float src_x = (static_cast<float>(x) + 0.5f) * scale_x - 0.5f;
                float src_y = (static_cast<float>(y) + 0.5f) * scale_y - 0.5f;
                for (size_t c = 0; c < channels; ++c) {
                    uint8_t v = sample_bilinear(image, src_x, src_y, static_cast<int>(c));
                    size_t offset = nchw ? c * input_h * input_w + y * input_w + x
                                         : (y * input_w + x) * channels + c;
                    dst[offset] = static_cast<float>(v) / 255.0f;
                }
            }
        }
    } else {
        return false;
    }

    hrt::sync(tensor, sync_op_t::sync_write_back, true)
        .expect("sync input failed");
    std::printf("YOLOV8N_DEMO: preprocess layout=%s input=%zux%zu\n",
                nchw ? "NCHW" : "NHWC", input_w, input_h);
    return true;
}

static float iou(const Detection &a, const Detection &b) {
    float x1 = std::max(a.x1, b.x1);
    float y1 = std::max(a.y1, b.y1);
    float x2 = std::min(a.x2, b.x2);
    float y2 = std::min(a.y2, b.y2);
    float w = std::max(0.0f, x2 - x1);
    float h = std::max(0.0f, y2 - y1);
    float inter = w * h;
    float area_a = std::max(0.0f, a.x2 - a.x1) * std::max(0.0f, a.y2 - a.y1);
    float area_b = std::max(0.0f, b.x2 - b.x1) * std::max(0.0f, b.y2 - b.y1);
    float denom = area_a + area_b - inter;
    return denom > 0.0f ? inter / denom : 0.0f;
}

static std::vector<Detection> nms(std::vector<Detection> detections,
                                  float threshold) {
    std::sort(detections.begin(), detections.end(),
              [](const Detection &a, const Detection &b) {
                  return a.score > b.score;
              });
    std::vector<Detection> kept;
    for (const auto &det : detections) {
        bool suppress = false;
        for (const auto &prev : kept) {
            if (det.class_id == prev.class_id && iou(det, prev) > threshold) {
                suppress = true;
                break;
            }
        }
        if (!suppress) {
            kept.push_back(det);
        }
        if (kept.size() >= 100) {
            break;
        }
    }
    return kept;
}

static std::vector<Detection> postprocess_yolov8(const float *data,
                                                 const dims_t &shape,
                                                 size_t bytes,
                                                 int image_w,
                                                 int image_h,
                                                 int input_w,
                                                 int input_h) {
    if (shape.size() < 2 || bytes < sizeof(float) * 5) {
        return {};
    }
    size_t a = shape[shape.size() - 2];
    size_t b = shape[shape.size() - 1];
    bool ncw = a <= b;
    size_t channels = ncw ? a : b;
    size_t rows = ncw ? b : a;
    if (channels < 5 || rows == 0) {
        return {};
    }
    size_t classes = std::min<size_t>(channels - 4, std::size(kCocoClasses));
    float x_factor = static_cast<float>(image_w) / static_cast<float>(input_w);
    float y_factor = static_cast<float>(image_h) / static_cast<float>(input_h);
    std::vector<Detection> candidates;
    std::vector<ScoreCandidate> top_scores;

    auto value_at = [&](size_t r, size_t c) -> float {
        return ncw ? data[c * rows + r] : data[r * channels + c];
    };

    auto remember_score = [&](const ScoreCandidate &candidate) {
        auto insert_at = top_scores.begin();
        while (insert_at != top_scores.end() && insert_at->score >= candidate.score) {
            ++insert_at;
        }
        top_scores.insert(insert_at, candidate);
        if (top_scores.size() > 5) {
            top_scores.pop_back();
        }
    };

    for (size_t r = 0; r < rows; ++r) {
        int best_class = -1;
        float best_score = -INFINITY;
        for (size_t c = 0; c < classes; ++c) {
            float score = value_at(r, 4 + c);
            if (std::isfinite(score) && score > best_score) {
                best_score = score;
                best_class = static_cast<int>(c);
            }
        }
        float x = value_at(r, 0);
        float y = value_at(r, 1);
        float w = value_at(r, 2);
        float h = value_at(r, 3);
        if (best_class >= 0 && std::isfinite(best_score)) {
            remember_score({r, best_class, best_score, x, y, w, h});
        }
        if (best_class < 0 || best_score < kScoreThreshold) {
            continue;
        }
        if (!std::isfinite(x) || !std::isfinite(y) || !std::isfinite(w) ||
            !std::isfinite(h) || w <= 0.0f || h <= 0.0f) {
            continue;
        }
        Detection det;
        det.x1 = std::max(0.0f, (x - 0.5f * w) * x_factor);
        det.y1 = std::max(0.0f, (y - 0.5f * h) * y_factor);
        det.x2 = std::min(static_cast<float>(image_w), (x + 0.5f * w) * x_factor);
        det.y2 = std::min(static_cast<float>(image_h), (y + 0.5f * h) * y_factor);
        det.score = best_score;
        det.class_id = best_class;
        if (det.x2 > det.x1 && det.y2 > det.y1) {
            candidates.push_back(det);
        }
    }

    std::printf("YOLOV8N_DEMO: postprocess rows=%zu channels=%zu candidates=%zu\n",
                rows, channels, candidates.size());
    for (size_t i = 0; i < top_scores.size(); ++i) {
        const auto &candidate = top_scores[i];
        const char *name = candidate.class_id >= 0 &&
                                   static_cast<size_t>(candidate.class_id) <
                                       std::size(kCocoClasses)
                               ? kCocoClasses[candidate.class_id]
                               : "unknown";
        std::printf(
            "YOLOV8N_DEMO: score_top[%zu] row=%zu class=%s score=%.6f box_raw=(%.3f,%.3f,%.3f,%.3f)\n",
            i, candidate.row, name, candidate.score, candidate.x, candidate.y,
            candidate.w, candidate.h);
    }
    return nms(std::move(candidates), kNmsThreshold);
}

static bool write_ppm(const char *path, const Image &image,
                      const std::vector<Detection> &detections) {
    std::vector<uint8_t> out = image.rgb;
    auto put_pixel = [&](int x, int y, uint8_t r, uint8_t g, uint8_t b) {
        if (x < 0 || y < 0 || x >= image.width || y >= image.height) {
            return;
        }
        size_t idx = (static_cast<size_t>(y) * image.width + x) * 3;
        out[idx] = r;
        out[idx + 1] = g;
        out[idx + 2] = b;
    };
    for (const auto &det : detections) {
        int x1 = static_cast<int>(det.x1);
        int y1 = static_cast<int>(det.y1);
        int x2 = static_cast<int>(det.x2);
        int y2 = static_cast<int>(det.y2);
        for (int x = x1; x <= x2; ++x) {
            put_pixel(x, y1, 255, 0, 0);
            put_pixel(x, y2, 255, 0, 0);
        }
        for (int y = y1; y <= y2; ++y) {
            put_pixel(x1, y, 255, 0, 0);
            put_pixel(x2, y, 255, 0, 0);
        }
    }

    FILE *fp = std::fopen(path, "wb");
    if (!fp) {
        return false;
    }
    std::fprintf(fp, "P6\n%d %d\n255\n", image.width, image.height);
    size_t wrote = std::fwrite(out.data(), 1, out.size(), fp);
    std::fclose(fp);
    return wrote == out.size();
}

static int run_demo(const char *kmodel_path, const char *image_path) {
    if (k230_compat_init() != 0) {
        std::printf("YOLOV8N_DEMO_FAIL: cannot initialize /dev/kpu compat\n");
        return 1;
    }

    Image image;
    if (!decode_jpeg(image_path, image)) {
        std::printf("YOLOV8N_DEMO_FAIL: decode failed: %s\n", image_path);
        return 1;
    }
    std::printf("YOLOV8N_DEMO: decode image=%s width=%d height=%d\n", image_path,
                image.width, image.height);

    std::vector<uint8_t> model = read_file(kmodel_path);
    if (model.empty()) {
        std::printf("YOLOV8N_DEMO_FAIL: cannot read kmodel: %s\n", kmodel_path);
        return 1;
    }

    interpreter interp;
    gsl::span<const gsl::byte> model_span(
        reinterpret_cast<const gsl::byte *>(model.data()), model.size());
    interp.load_model(model_span, true).expect("load_model failed");
    std::printf("YOLOV8N_DEMO: load_model ok inputs=%zu outputs=%zu\n",
                interp.inputs_size(), interp.outputs_size());
    if (interp.inputs_size() == 0 || interp.outputs_size() == 0) {
        std::printf("YOLOV8N_DEMO_FAIL: model has no input/output tensors\n");
        return 1;
    }

    auto input_desc = interp.input_desc(0);
    auto input_shape = interp.input_shape(0);
    auto input_tensor =
        host_runtime_tensor::create(input_desc.datatype, input_shape,
                                    hrt::pool_shared)
            .expect("cannot create input tensor");
    std::printf("YOLOV8N_DEMO: input[0] datatype=%u shape=",
                static_cast<unsigned>(input_desc.datatype));
    print_shape(input_shape);
    std::printf("\n");
    if (!fill_input_from_image(input_tensor, input_shape, input_desc.datatype,
                               image)) {
        std::printf("YOLOV8N_DEMO_FAIL: unsupported input tensor layout/type\n");
        return 1;
    }
    if (!mirror_tensor_to_direct(input_tensor, KPU_RUNTIME_DIRECT_SOURCE_PADDR,
                                 "input_source")) {
        return 1;
    }
    interp.input_tensor(0, input_tensor).expect("cannot set input tensor");

    std::vector<runtime_tensor> output_tensors;
    for (size_t i = 0; i < interp.outputs_size(); ++i) {
        auto desc = interp.output_desc(i);
        auto shape = interp.output_shape(i);
        auto tensor =
            host_runtime_tensor::create(desc.datatype, shape, hrt::pool_shared)
                .expect("cannot create output tensor");
        std::printf("YOLOV8N_DEMO: output[%zu] datatype=%u shape=", i,
                    static_cast<unsigned>(desc.datatype));
        print_shape(shape);
        std::printf(" elements=%zu\n", compute_size(shape));
        interp.output_tensor(i, tensor).expect("cannot set output tensor");
        output_tensors.push_back(tensor);
    }

    std::printf("YOLOV8N_DEMO: run\n");
    interp.run().expect("interp.run failed");
    std::printf("YOLOV8N_DEMO: run done\n");

    for (size_t i = 0; i < interp.outputs_size(); ++i) {
        auto output = interp.output_tensor(i).expect("cannot get output tensor");
        auto output_desc = interp.output_desc(i);
        auto mapped = output.impl()
                          ->to_host()
                          .unwrap()
                          ->buffer()
                          .as_host()
                          .unwrap()
                          .map(map_access_::map_read)
                          .unwrap()
                          .buffer();
        const auto *out_bytes = reinterpret_cast<const uint8_t *>(mapped.data());
        print_runtime_paddr("YOLOV8N_DEMO: output", i, mapped.data(),
                            mapped.size_bytes());
        uint64_t hash = fnv1a64(out_bytes, mapped.size_bytes());
        std::printf("YOLOV8N_DEMO: output[%zu] bytes=%zu fnv1a64=0x%016llx\n",
                    i, mapped.size_bytes(), static_cast<unsigned long long>(hash));
        if (output_desc.datatype == dt_float32) {
            auto stats = summarize_floats(reinterpret_cast<const float *>(mapped.data()),
                                          mapped.size_bytes() / sizeof(float));
            std::printf(
                "YOLOV8N_DEMO: output[%zu] stats finite=%zu nan=%zu min=%.6f max=%.6f mean=%.6f\n",
                i, stats.finite, stats.nan, stats.min, stats.max, stats.mean);
        }
    }
    inspect_direct_yolo_output();

    auto output = interp.output_tensor(0).expect("cannot get output tensor");
    auto output_desc = interp.output_desc(0);
    auto output_shape = interp.output_shape(0);
    auto mapped = output.impl()
                      ->to_host()
                      .unwrap()
                      ->buffer()
                      .as_host()
                      .unwrap()
                      .map(map_access_::map_read)
                      .unwrap()
                      .buffer();

    size_t input_h = input_shape[1] == 3 ? input_shape[2] : input_shape[1];
    size_t input_w = input_shape[1] == 3 ? input_shape[3] : input_shape[2];
    std::vector<Detection> detections;
    if (output_desc.datatype == dt_float32) {
        std::printf("YOLOV8N_DEMO: postprocess threshold score=%.2f nms=%.2f\n",
                    kScoreThreshold, kNmsThreshold);
        detections = postprocess_yolov8(
            reinterpret_cast<const float *>(mapped.data()), output_shape,
            mapped.size_bytes(), image.width, image.height,
            static_cast<int>(input_w), static_cast<int>(input_h));
    } else {
        std::printf("YOLOV8N_DEMO: postprocess skipped datatype=%u\n",
                    static_cast<unsigned>(output_desc.datatype));
    }

    std::printf("YOLOV8N_DEMO: detections=%zu\n", detections.size());
    if (!detections.empty()) {
        const auto &top = detections.front();
        const char *name = top.class_id >= 0 &&
                                   static_cast<size_t>(top.class_id) <
                                       std::size(kCocoClasses)
                               ? kCocoClasses[top.class_id]
                               : "unknown";
        std::printf(
            "YOLOV8N_DEMO: top class=%s score=%.5f box=(%.1f,%.1f,%.1f,%.1f)\n",
            name, top.score, top.x1, top.y1, top.x2, top.y2);
    } else {
        std::printf("YOLOV8N_DEMO: top none\n");
    }
    if (write_ppm("/tmp/k230-yolov8n-demo.ppm", image, detections)) {
        std::printf("YOLOV8N_DEMO: annotated=/tmp/k230-yolov8n-demo.ppm\n");
    }
    k230_compat_dump_stats();
    std::printf("YOLOV8N_DEMO_PASS\n");
    std::fflush(nullptr);
    // The official K230 SDK MMZ allocator can assert while tearing down process
    // globals under Starry/Linux ABI. The run has completed once PASS is printed.
    std::_Exit(0);
    return 0;
}

} // namespace

int main(int argc, char *argv[]) {
    if (argc != 3) {
        std::printf("YOLOV8N_DEMO_FAIL: usage: %s <kmodel> <bus.jpg>\n",
                    argv[0]);
        return 2;
    }
    try {
        return run_demo(argv[1], argv[2]);
    } catch (const std::exception &ex) {
        std::printf("YOLOV8N_DEMO_FAIL: exception: %s\n", ex.what());
        return 1;
    } catch (...) {
        std::printf("YOLOV8N_DEMO_FAIL: unknown exception\n");
        return 1;
    }
}
