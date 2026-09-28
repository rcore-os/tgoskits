#include "detection_validation.h"

#include <errno.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <algorithm>
#include <fstream>
#include <sstream>

namespace rknn_validation {

namespace {

const double kMinIou = 0.80;
const int kScoreToleranceQ10000 = 2000;

void SetError(std::string *error, const std::string &message)
{
    if (error != NULL) {
        *error = message;
    }
}

std::string Trim(const std::string &value)
{
    size_t begin = 0;
    while (begin < value.size() && (value[begin] == ' ' || value[begin] == '\t' ||
                                    value[begin] == '\r' || value[begin] == '\n')) {
        begin++;
    }
    size_t end = value.size();
    while (end > begin && (value[end - 1] == ' ' || value[end - 1] == '\t' ||
                           value[end - 1] == '\r' || value[end - 1] == '\n')) {
        end--;
    }
    return value.substr(begin, end - begin);
}

std::vector<std::string> SplitTokens(const std::string &line)
{
    std::vector<std::string> tokens;
    std::istringstream stream(line);
    std::string token;
    while (stream >> token) {
        tokens.push_back(token);
    }
    return tokens;
}

std::string TokenValue(const std::vector<std::string> &tokens, const char *key)
{
    const size_t key_len = strlen(key);
    for (size_t i = 0; i < tokens.size(); i++) {
        const std::string &token = tokens[i];
        if (token.compare(0, key_len, key) == 0 && token.size() > key_len && token[key_len] == '=') {
            return token.substr(key_len + 1);
        }
    }
    return "";
}

bool ParseIntValue(const std::string &text, int *out)
{
    if (text.empty()) {
        return false;
    }
    char *end = NULL;
    errno = 0;
    long value = strtol(text.c_str(), &end, 10);
    if (errno != 0 || end == text.c_str() || *end != '\0' ||
        value < -2147483647L || value > 2147483647L) {
        return false;
    }
    *out = (int)value;
    return true;
}

bool ParseRequiredInt(const std::vector<std::string> &tokens, const char *key, int *out, std::string *error)
{
    const std::string value = TokenValue(tokens, key);
    if (!ParseIntValue(value, out)) {
        std::ostringstream message;
        message << "invalid or missing " << key;
        SetError(error, message.str());
        return false;
    }
    return true;
}

bool ReadFile(const std::string &path, std::string *content, std::string *error)
{
    std::ifstream file(path.c_str(), std::ios::in | std::ios::binary);
    if (!file) {
        std::ostringstream message;
        message << "open failed path=" << path;
        SetError(error, message.str());
        return false;
    }
    std::ostringstream buffer;
    buffer << file.rdbuf();
    *content = buffer.str();
    return true;
}

bool WriteFile(const std::string &path, const std::string &content, std::string *error)
{
    std::ofstream file(path.c_str(), std::ios::out | std::ios::binary | std::ios::trunc);
    if (!file) {
        std::ostringstream message;
        message << "open for write failed path=" << path;
        SetError(error, message.str());
        return false;
    }
    file << content;
    if (!file) {
        std::ostringstream message;
        message << "write failed path=" << path;
        SetError(error, message.str());
        return false;
    }
    return true;
}

double BoxArea(const DetectionEntry &box)
{
    const int width = std::max(0, box.right - box.left);
    const int height = std::max(0, box.bottom - box.top);
    return (double)width * (double)height;
}

int ClampScoreQ10000(int score)
{
    if (score < 0) {
        return 0;
    }
    if (score > 10000) {
        return 10000;
    }
    return score;
}

std::string DetectionSummary(const DetectionEntry &det)
{
    std::ostringstream message;
    message << "cls=" << det.cls_id
            << " score_q10000=" << det.score_q10000
            << " box=(" << det.left << "," << det.top << "," << det.right << "," << det.bottom << ")";
    return message.str();
}

void AppendMessage(std::vector<std::string> *messages, const std::string &message)
{
    if (messages != NULL) {
        messages->push_back(message);
    }
}

}  // namespace

DetectionEntry::DetectionEntry()
    : cls_id(0), score_q10000(0), left(0), top(0), right(0), bottom(0)
{
}

DetectionEntry::DetectionEntry(int cls_id_value, int score_value, int left_value, int top_value,
                               int right_value, int bottom_value)
    : cls_id(cls_id_value),
      score_q10000(score_value),
      left(left_value),
      top(top_value),
      right(right_value),
      bottom(bottom_value)
{
}

ValidationImage::ValidationImage()
    : index(0), width(0), height(0)
{
}

ExpectedImage::ExpectedImage()
    : index(0), width(0), height(0)
{
}

ExpectedFile::ExpectedFile()
    : version(1), min_confidence(25), nms_threshold_q10000(4500)
{
}

bool ParseImageList(const std::string &content, std::vector<ValidationImage> *images, std::string *error)
{
    if (images == NULL) {
        SetError(error, "invalid image list output");
        return false;
    }
    images->clear();

    std::istringstream stream(content);
    std::string line;
    int line_no = 0;
    while (std::getline(stream, line)) {
        line_no++;
        const std::string item = Trim(line);
        if (item.empty() || item[0] == '#') {
            continue;
        }
        ValidationImage image;
        image.index = (int)images->size();
        image.path = item;
        images->push_back(image);
    }

    if (images->empty()) {
        std::ostringstream message;
        message << "image list is empty lines=" << line_no;
        SetError(error, message.str());
        return false;
    }
    return true;
}

std::string WriteImageList(const std::vector<ValidationImage> &images)
{
    std::ostringstream out;
    for (size_t i = 0; i < images.size(); i++) {
        out << images[i].path << "\n";
    }
    return out.str();
}

bool ReadImageListFile(const std::string &path, std::vector<ValidationImage> *images, std::string *error)
{
    std::string content;
    if (!ReadFile(path, &content, error)) {
        return false;
    }
    return ParseImageList(content, images, error);
}

bool WriteImageListFile(const std::string &path, const std::vector<ValidationImage> &images, std::string *error)
{
    return WriteFile(path, WriteImageList(images), error);
}

bool ParseExpectedFile(const std::string &content, ExpectedFile *expected, std::string *error)
{
    if (expected == NULL) {
        SetError(error, "invalid expected output");
        return false;
    }

    ExpectedFile parsed;
    std::istringstream stream(content);
    std::string line;
    int line_no = 0;
    int declared_image_count = -1;
    std::vector<int> declared_detection_counts;
    bool saw_header = false;

    while (std::getline(stream, line)) {
        line_no++;
        const std::string text = Trim(line);
        if (text.empty() || text[0] == '#') {
            continue;
        }
        const std::vector<std::string> tokens = SplitTokens(text);
        if (tokens.empty()) {
            continue;
        }

        if (tokens[0] == "RKNN_VALIDATE_EXPECTED") {
            saw_header = true;
            if (!ParseRequiredInt(tokens, "version", &parsed.version, error) ||
                !ParseRequiredInt(tokens, "image_count", &declared_image_count, error) ||
                !ParseRequiredInt(tokens, "min_confidence", &parsed.min_confidence, error) ||
                !ParseRequiredInt(tokens, "nms_threshold_q10000", &parsed.nms_threshold_q10000, error)) {
                return false;
            }
            if (parsed.version != 1) {
                SetError(error, "unsupported expected version");
                return false;
            }
        } else if (tokens[0] == "image") {
            ExpectedImage image;
            int declared_count = 0;
            if (!ParseRequiredInt(tokens, "index", &image.index, error) ||
                !ParseRequiredInt(tokens, "width", &image.width, error) ||
                !ParseRequiredInt(tokens, "height", &image.height, error) ||
                !ParseRequiredInt(tokens, "count", &declared_count, error)) {
                return false;
            }
            image.path = TokenValue(tokens, "path");
            if (image.path.empty() || declared_count < 0) {
                SetError(error, "invalid image line");
                return false;
            }
            parsed.images.push_back(image);
            declared_detection_counts.push_back(declared_count);
        } else if (tokens[0] == "det") {
            int image_index = 0;
            DetectionEntry det;
            if (!ParseRequiredInt(tokens, "image", &image_index, error) ||
                !ParseRequiredInt(tokens, "cls", &det.cls_id, error) ||
                !ParseRequiredInt(tokens, "score_q10000", &det.score_q10000, error) ||
                !ParseRequiredInt(tokens, "left", &det.left, error) ||
                !ParseRequiredInt(tokens, "top", &det.top, error) ||
                !ParseRequiredInt(tokens, "right", &det.right, error) ||
                !ParseRequiredInt(tokens, "bottom", &det.bottom, error)) {
                return false;
            }

            bool appended = false;
            for (size_t i = 0; i < parsed.images.size(); i++) {
                if (parsed.images[i].index == image_index) {
                    parsed.images[i].detections.push_back(det);
                    appended = true;
                    break;
                }
            }
            if (!appended) {
                std::ostringstream message;
                message << "det references unknown image index=" << image_index << " line=" << line_no;
                SetError(error, message.str());
                return false;
            }
        } else {
            std::ostringstream message;
            message << "unknown expected line=" << line_no << " text=" << text;
            SetError(error, message.str());
            return false;
        }
    }

    if (!saw_header) {
        SetError(error, "missing expected header");
        return false;
    }
    if (declared_image_count != (int)parsed.images.size()) {
        std::ostringstream message;
        message << "image_count mismatch declared=" << declared_image_count << " parsed=" << parsed.images.size();
        SetError(error, message.str());
        return false;
    }
    for (size_t i = 0; i < parsed.images.size(); i++) {
        if (parsed.images[i].index != (int)i) {
            std::ostringstream message;
            message << "image index must be sequential expected=" << i << " actual=" << parsed.images[i].index;
            SetError(error, message.str());
            return false;
        }
        if (i >= declared_detection_counts.size() ||
            (int)parsed.images[i].detections.size() != declared_detection_counts[i]) {
            std::ostringstream message;
            message << "detection count mismatch image=" << parsed.images[i].index
                    << " declared=" << (i < declared_detection_counts.size() ? declared_detection_counts[i] : -1)
                    << " parsed=" << parsed.images[i].detections.size();
            SetError(error, message.str());
            return false;
        }
    }

    *expected = parsed;
    return true;
}

std::string WriteExpectedFile(const ExpectedFile &expected)
{
    std::ostringstream out;
    out << "RKNN_VALIDATE_EXPECTED version=" << expected.version
        << " image_count=" << expected.images.size()
        << " min_confidence=" << expected.min_confidence
        << " nms_threshold_q10000=" << expected.nms_threshold_q10000 << "\n";

    for (size_t i = 0; i < expected.images.size(); i++) {
        const ExpectedImage &image = expected.images[i];
        out << "image index=" << image.index
            << " path=" << image.path
            << " width=" << image.width
            << " height=" << image.height
            << " count=" << image.detections.size() << "\n";
        for (size_t j = 0; j < image.detections.size(); j++) {
            const DetectionEntry &det = image.detections[j];
            out << "det image=" << image.index
                << " cls=" << det.cls_id
                << " score_q10000=" << det.score_q10000
                << " left=" << det.left
                << " top=" << det.top
                << " right=" << det.right
                << " bottom=" << det.bottom << "\n";
        }
    }
    return out.str();
}

bool ReadExpectedFile(const std::string &path, ExpectedFile *expected, std::string *error)
{
    std::string content;
    if (!ReadFile(path, &content, error)) {
        return false;
    }
    return ParseExpectedFile(content, expected, error);
}

bool WriteExpectedFile(const std::string &path, const ExpectedFile &expected, std::string *error)
{
    return WriteFile(path, WriteExpectedFile(expected), error);
}

std::vector<DetectionEntry> ConvertDetections(const object_detect_result_list &results)
{
    std::vector<DetectionEntry> detections;
    for (int i = 0; i < results.count && i < OBJ_NUMB_MAX_SIZE; i++) {
        const object_detect_result &result = results.results[i];
        detections.push_back(DetectionEntry(
            result.cls_id,
            ClampScoreQ10000((int)lround(result.prop * 10000.0f)),
            result.box.left,
            result.box.top,
            result.box.right,
            result.box.bottom));
    }
    return detections;
}

double DetectionIoU(const DetectionEntry &a, const DetectionEntry &b)
{
    const int left = std::max(a.left, b.left);
    const int top = std::max(a.top, b.top);
    const int right = std::min(a.right, b.right);
    const int bottom = std::min(a.bottom, b.bottom);
    const double intersection = BoxArea(DetectionEntry(0, 0, left, top, right, bottom));
    const double union_area = BoxArea(a) + BoxArea(b) - intersection;
    if (union_area <= 0.0) {
        return 0.0;
    }
    return intersection / union_area;
}

bool ValidateDetections(const ExpectedImage &expected, const std::vector<DetectionEntry> &actual,
                        std::vector<std::string> *messages)
{
    if (messages != NULL) {
        messages->clear();
    }
    bool ok = true;
    if (expected.detections.size() != actual.size()) {
        std::ostringstream message;
        message << "image index=" << expected.index
                << " path=" << expected.path
                << " count mismatch expected=" << expected.detections.size()
                << " actual=" << actual.size();
        AppendMessage(messages, message.str());
        ok = false;
    }

    std::vector<bool> used(actual.size(), false);
    for (size_t i = 0; i < expected.detections.size(); i++) {
        const DetectionEntry &want = expected.detections[i];
        int best_index = -1;
        double best_iou = -1.0;
        for (size_t j = 0; j < actual.size(); j++) {
            if (used[j] || actual[j].cls_id != want.cls_id) {
                continue;
            }
            const double iou = DetectionIoU(want, actual[j]);
            if (iou > best_iou) {
                best_iou = iou;
                best_index = (int)j;
            }
        }

        if (best_index < 0) {
            std::ostringstream message;
            message << "image index=" << expected.index
                    << " path=" << expected.path
                    << " cls mismatch expected_det=" << i
                    << " expected " << DetectionSummary(want);
            AppendMessage(messages, message.str());
            ok = false;
            continue;
        }

        const DetectionEntry &got = actual[(size_t)best_index];
        const int score_delta = abs(want.score_q10000 - got.score_q10000);
        if (best_iou < kMinIou) {
            std::ostringstream message;
            message << "image index=" << expected.index
                    << " path=" << expected.path
                    << " iou below threshold expected_det=" << i
                    << " actual_det=" << best_index
                    << " cls=" << want.cls_id
                    << " iou=" << best_iou
                    << " min_iou=" << kMinIou
                    << " expected " << DetectionSummary(want)
                    << " actual " << DetectionSummary(got);
            AppendMessage(messages, message.str());
            ok = false;
        }
        if (score_delta > kScoreToleranceQ10000) {
            std::ostringstream message;
            message << "image index=" << expected.index
                    << " path=" << expected.path
                    << " score delta too large expected_det=" << i
                    << " actual_det=" << best_index
                    << " expected_score_q10000=" << want.score_q10000
                    << " actual_score_q10000=" << got.score_q10000
                    << " delta=" << score_delta
                    << " tolerance=" << kScoreToleranceQ10000;
            AppendMessage(messages, message.str());
            ok = false;
        }

        used[(size_t)best_index] = true;
    }

    for (size_t i = 0; i < actual.size(); i++) {
        if (!used[i]) {
            std::ostringstream message;
            message << "image index=" << expected.index
                    << " path=" << expected.path
                    << " unexpected detection actual_det=" << i
                    << " " << DetectionSummary(actual[i]);
            AppendMessage(messages, message.str());
            ok = false;
        }
    }

    return ok;
}

const ExpectedImage *FindExpectedImage(const ExpectedFile &expected, int image_index)
{
    for (size_t i = 0; i < expected.images.size(); i++) {
        if (expected.images[i].index == image_index) {
            return &expected.images[i];
        }
    }
    return NULL;
}

bool ValidateImageDetections(const ExpectedFile &expected, int image_index,
                             const std::vector<DetectionEntry> &actual,
                             std::vector<std::string> *messages)
{
    const ExpectedImage *image = FindExpectedImage(expected, image_index);
    if (image == NULL) {
        if (messages != NULL) {
            std::ostringstream message;
            message << "missing expected image index=" << image_index;
            messages->push_back(message.str());
        }
        return false;
    }
    return ValidateDetections(*image, actual, messages);
}

}  // namespace rknn_validation
