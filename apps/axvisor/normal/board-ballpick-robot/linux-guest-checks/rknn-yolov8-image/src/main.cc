// SPDX-License-Identifier: Apache-2.0
// Directory-only Linux demo using the RKNN code shipped in this package.
#include <algorithm>
#include <cerrno>
#include <cctype>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <dirent.h>
#include <memory>
#include <stdexcept>
#include <string>
#include <sys/stat.h>
#include <vector>
#include "yolov8.h"

static std::vector<std::string> image_paths(const char *directory)
{
    std::unique_ptr<DIR, int (*)(DIR *)> dir(opendir(directory), closedir);
    if (!dir) throw std::runtime_error(std::string("Cannot open image directory: ") + strerror(errno));
    std::vector<std::string> paths;
    for (;;) {
        errno = 0;
        dirent *entry = readdir(dir.get());
        if (!entry) {
            if (errno) throw std::runtime_error("Cannot read image directory");
            break;
        }
        std::string name(entry->d_name);
        const auto dot = name.find_last_of('.');
        if (dot == std::string::npos) continue;
        std::string ext = name.substr(dot);
        for (char &c : ext) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
        if (ext != ".jpg" && ext != ".jpeg" && ext != ".png") continue;
        std::string path = std::string(directory) + "/" + name;
        struct stat info;
        if (stat(path.c_str(), &info) != 0) throw std::runtime_error("Cannot access image: " + path);
        if (S_ISREG(info.st_mode)) paths.push_back(path);
    }
    std::sort(paths.begin(), paths.end());
    if (paths.empty()) throw std::runtime_error("No JPG, JPEG or PNG images found");
    return paths;
}

static void check_labels(const char *path)
{
    std::unique_ptr<FILE, int (*)(FILE *)> file(fopen(path, "r"), fclose);
    if (!file) throw std::runtime_error("Cannot open COCO labels");
    char line[256];
    unsigned count = 0;
    while (fgets(line, sizeof(line), file.get())) {
        if (!strchr(line, '\n') && !feof(file.get())) throw std::runtime_error("Label is too long");
        line[strcspn(line, "\r\n")] = '\0';
        if (!line[0]) throw std::runtime_error("Empty COCO label");
        ++count;
    }
    if (ferror(file.get()) || count != OBJ_CLASS_NUM) throw std::runtime_error("Expected 80 COCO labels");
}

struct Model {
    rknn_app_context_t context{};
    ~Model() { release_yolov8_model(&context); deinit_post_process(); }
};

static int run(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--help") == 0) {
        puts("Usage: rknn_yolov8_images MODEL LABELS IMAGE_DIRECTORY [CONFIDENCE_PERCENT]");
        return 0;
    }
    if (argc < 4 || argc > 5) throw std::runtime_error("Use --help for usage");
    char *end = nullptr;
    errno = 0;
    const double confidence = argc == 5 ? strtod(argv[4], &end) : 25.0;
    if (!std::isfinite(confidence) || confidence < 0 || confidence > 100 ||
        (argc == 5 && (end == argv[4] || *end || errno)))
        throw std::runtime_error("Confidence must be a number between 0 and 100");
    const auto images = image_paths(argv[3]);
    check_labels(argv[2]);
    puts("========== NPU YOLOv8 Image Inference ==========");
    printf("Model: %s\nInput directory: %s\nImages: %zu\nConfidence: %.1f%%\nNPU core: 0\n\n",
           argv[1], argv[3], images.size(), confidence);
    Model model;
    if (init_post_process(argv[2]) != 0 || init_yolov8_model(argv[1], &model.context) != 0)
        throw std::runtime_error("Model initialization failed; check RKNN model/runtime/driver compatibility");
    if (rknn_set_core_mask(model.context.rknn_ctx, RKNN_NPU_CORE_0) != RKNN_SUCC)
        throw std::runtime_error("Cannot select NPU core 0");
    size_t passed = 0;
    for (size_t i = 0; i < images.size(); ++i) {
        printf("\n[%zu/%zu] %s\n", i + 1, images.size(), images[i].c_str());
        image_buffer_t image{};
        const int decoded = read_image(images[i].c_str(), &image);
        std::unique_ptr<unsigned char, void (*)(void *)> pixels(image.virt_addr, free);
        if (decoded != 0 || !image.virt_addr || image.width <= 0 || image.height <= 0) {
            puts("Status: FAIL (image decode failed)");
            continue;
        }
        printf("Image size: %d x %d\n", image.width, image.height);
        object_detect_result_list results{};
        rknn_inference_profile_t profile{};
        const int status = inference_yolov8_model_with_thresholds_profile(
            &model.context, &image, &results, static_cast<float>(confidence / 100), NMS_THRESH, &profile);
        if (status != 0 || results.count < 0 || results.count > OBJ_NUMB_MAX_SIZE) {
            printf("Status: FAIL (inference/postprocessing, code=%d)\n", status);
            continue;
        }
        bool valid = true;
        for (int n = 0; n < results.count; ++n) {
            const auto &d = results.results[n];
            valid &= d.cls_id >= 0 && d.cls_id < OBJ_CLASS_NUM && std::isfinite(d.prop) && d.prop >= 0 && d.prop <= 1;
        }
        if (!valid) { puts("Status: FAIL (invalid detection data)"); continue; }
        printf("Detections: %d\n", results.count);
        if (results.count) puts("ID  Class ID  Class            Confidence  Box (left, top, right, bottom)");
        for (int n = 0; n < results.count; ++n) {
            const auto &d = results.results[n];
            printf("%-3d %-9d %-16s %6.2f%%    (%d, %d, %d, %d)\n", n + 1, d.cls_id,
                   coco_cls_to_name(d.cls_id), d.prop * 100, d.box.left, d.box.top, d.box.right, d.box.bottom);
        }
        printf("NPU run call: %.3f ms\nPreprocess + inference + postprocess: %.3f ms\nStatus: PASS\n",
               profile.run_ms, profile.total_ms);
        ++passed;
    }
    printf("\n========== Test Summary ==========\nProcessed: %zu\nPassed: %zu\nFailed: %zu\n",
           images.size(), passed, images.size() - passed);
    if (passed != images.size()) return 1;
    puts("YOLO_IMAGE_LIST_SUCCESS\nsuccess");
    return 0;
}

int main(int argc, char **argv)
{
    setvbuf(stdout, nullptr, _IOLBF, 0);
    try { return run(argc, argv); }
    catch (const std::exception &error) {
        fprintf(stderr, "Status: FAIL (%s)\n", error.what());
        return 1;
    }
}
