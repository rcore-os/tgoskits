#include <stdint.h>

#include <chrono>
#include <cmath>
#include <fstream>
#include <string>
#include <vector>

#include "ncnn/mat.h"
#include "ncnn/net.h"

extern "C" {
struct Task3NcnnDetection {
    uint16_t class_id;
    uint16_t confidence_milli;
    uint16_t center_x_milli;
    uint16_t area_milli;
};
}

namespace {

static bool read_file(const char* path, std::vector<unsigned char>& bytes) {
    std::ifstream input(path, std::ios::binary);
    if (!input) return false;
    input.seekg(0, std::ios::end);
    const std::streamoff size = input.tellg();
    if (size <= 0) return false;
    input.seekg(0, std::ios::beg);
    bytes.resize(static_cast<size_t>(size));
    input.read(reinterpret_cast<char*>(bytes.data()), size);
    return input.good() || input.eof();
}

static bool next_token(const std::vector<unsigned char>& bytes, size_t& cursor,
                       std::string& token) {
    while (cursor < bytes.size() && bytes[cursor] <= ' ') cursor++;
    if (cursor >= bytes.size()) return false;
    if (bytes[cursor] == '#') {
        while (cursor < bytes.size() && bytes[cursor] != '\n') cursor++;
        return next_token(bytes, cursor, token);
    }
    const size_t begin = cursor;
    while (cursor < bytes.size() && bytes[cursor] > ' ') cursor++;
    token.assign(reinterpret_cast<const char*>(bytes.data() + begin), cursor - begin);
    return true;
}

static bool parse_positive_int(const std::string& token, int& out) {
    if (token.empty()) return false;
    int value = 0;
    for (size_t index = 0; index < token.size(); index++) {
        const unsigned char character = static_cast<unsigned char>(token[index]);
        if (character < '0' || character > '9') return false;
        if (value > 100000000 / 10) return false;
        value = value * 10 + (character - '0');
    }
    if (value <= 0) return false;
    out = value;
    return true;
}

static bool read_ppm(const char* path, std::vector<unsigned char>& pixels,
                     int& width, int& height) {
    std::vector<unsigned char> bytes;
    if (!read_file(path, bytes)) return false;
    size_t cursor = 0;
    std::string magic, width_token, height_token, max_token;
    if (!next_token(bytes, cursor, magic) || magic != "P6" ||
        !next_token(bytes, cursor, width_token) ||
        !next_token(bytes, cursor, height_token) ||
        !next_token(bytes, cursor, max_token)) return false;
    if (!parse_positive_int(width_token, width) ||
        !parse_positive_int(height_token, height) || max_token != "255") return false;
    if (width > 4096 || height > 4096) return false;
    while (cursor < bytes.size() && bytes[cursor] <= ' ') cursor++;
    const size_t expected = static_cast<size_t>(width) * static_cast<size_t>(height) * 3;
    if (bytes.size() - cursor != expected) return false;
    pixels.assign(bytes.begin() + cursor, bytes.end());
    return true;
}

static uint16_t milli(float value) {
    if (value <= 0.f) return 0;
    if (value >= 1.f) return 1000;
    return static_cast<uint16_t>(value * 1000.f + 0.5f);
}

}  // namespace

extern "C" int task3_ncnn_infer(
    const char* param_path, const char* model_path, const char* input_path,
    Task3NcnnDetection* detection, uint64_t* infer_us) {
    if (!param_path || !model_path || !input_path || !detection || !infer_us) return -1;

    std::vector<unsigned char> param_bytes;
    std::vector<unsigned char> model_bytes;
    std::vector<unsigned char> pixels;
    int width = 0;
    int height = 0;
    if (!read_file(param_path, param_bytes) || !read_file(model_path, model_bytes) ||
        !read_ppm(input_path, pixels, width, height)) return -2;
    param_bytes.push_back(0);

    ncnn::Net net;
    net.opt.use_vulkan_compute = false;
    net.opt.use_fp16_storage = false;
    net.opt.use_fp16_arithmetic = false;
    net.opt.num_threads = 1;
    if (net.load_param_mem(reinterpret_cast<const char*>(param_bytes.data())) != 0) return -3;
    if (net.load_model(model_bytes.data()) == 0) return -4;

    ncnn::Mat input = ncnn::Mat::from_pixels_resize(
        pixels.data(), ncnn::Mat::PIXEL_RGB, width, height, 640, 640);
    if (input.empty()) return -5;
    const float normalization[] = {1.f / 255.f, 1.f / 255.f, 1.f / 255.f};
    input.substract_mean_normalize(nullptr, normalization);

    ncnn::Extractor extractor = net.create_extractor();
    extractor.set_light_mode(true);
    if (extractor.input("in0", input) != 0) return -6;
    ncnn::Mat output;
    const auto begin = std::chrono::steady_clock::now();
    if (extractor.extract("out0", output) != 0) return -7;
    const auto end = std::chrono::steady_clock::now();
    *infer_us = static_cast<uint64_t>(
        std::chrono::duration_cast<std::chrono::microseconds>(end - begin).count());

    if (output.dims != 2 || output.w < 5 || output.h == 0) return -8;
    const int rows = output.w;
    const int channels = output.h;
    const float* output_values = output;
    for (int channel = 0; channel < channels; channel++) {
        for (int row = 0; row < rows; row++) {
            if (!std::isfinite(output_values[channel * rows + row])) return -9;
        }
    }
    int best_row = -1;
    int best_class = -1;
    float best_score = 0.f;
    for (int row = 0; row < rows; row++) {
        for (int class_id = 4; class_id < channels; class_id++) {
            const float score = output_values[class_id * rows + row];
            if (score > best_score) {
                best_score = score;
                best_row = row;
                best_class = class_id - 4;
            }
        }
    }
    if (best_row < 0 || best_score <= 0.f) return 1;
    const float center_x = output_values[best_row] / 640.f;
    const float box_width = output_values[2 * rows + best_row] / 640.f;
    const float box_height = output_values[3 * rows + best_row] / 640.f;
    detection->class_id = static_cast<uint16_t>(best_class);
    detection->confidence_milli = milli(best_score);
    detection->center_x_milli = milli(center_x);
    detection->area_milli = milli(box_width * box_height);
    return 0;
}
