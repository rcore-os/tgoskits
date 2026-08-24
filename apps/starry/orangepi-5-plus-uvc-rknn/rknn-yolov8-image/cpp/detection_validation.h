#ifndef RKNN_YOLOV8_IMAGE_DETECTION_VALIDATION_H_
#define RKNN_YOLOV8_IMAGE_DETECTION_VALIDATION_H_

#include <stdint.h>

#include <string>
#include <vector>

#include "yolov8.h"

namespace rknn_validation {

struct DetectionEntry {
    int cls_id;
    int score_q10000;
    int left;
    int top;
    int right;
    int bottom;

    DetectionEntry();
    DetectionEntry(int cls_id, int score_q10000, int left, int top, int right, int bottom);
};

enum class SortingAction {
    Hold = 0,
    SortLeft = 1,
    SortRight = 2,
};

struct SortingDecision {
    SortingAction action;
    bool detection_present;
    DetectionEntry detection;

    SortingDecision();
};

struct ValidationImage {
    int index;
    std::string path;
    int width;
    int height;

    ValidationImage();
};

struct ExpectedImage {
    int index;
    std::string path;
    int width;
    int height;
    std::vector<DetectionEntry> detections;

    ExpectedImage();
};

struct ExpectedFile {
    int version;
    int min_confidence;
    int nms_threshold_q10000;
    std::vector<ExpectedImage> images;

    ExpectedFile();
};

bool ParseImageList(const std::string &content, std::vector<ValidationImage> *images, std::string *error);
std::string WriteImageList(const std::vector<ValidationImage> &images);
bool ReadImageListFile(const std::string &path, std::vector<ValidationImage> *images, std::string *error);
bool WriteImageListFile(const std::string &path, const std::vector<ValidationImage> &images, std::string *error);

bool ParseExpectedFile(const std::string &content, ExpectedFile *expected, std::string *error);
std::string WriteExpectedFile(const ExpectedFile &expected);
bool ReadExpectedFile(const std::string &path, ExpectedFile *expected, std::string *error);
bool WriteExpectedFile(const std::string &path, const ExpectedFile &expected, std::string *error);

std::vector<DetectionEntry> ConvertDetections(const object_detect_result_list &results);

SortingDecision SelectSortingDecision(const std::vector<DetectionEntry> &detections,
                                      int target_class_id,
                                      int calibration_x);

const char *SortingActionName(SortingAction action);

std::string FormatVisionDecisionRecord(uint64_t frame_id,
                                       uint64_t captured_at_us,
                                       uint64_t inference_finished_at_us,
                                       uint32_t ttl_us,
                                       const SortingDecision &decision);

double DetectionIoU(const DetectionEntry &a, const DetectionEntry &b);
bool ValidateDetections(const ExpectedImage &expected, const std::vector<DetectionEntry> &actual,
                        std::vector<std::string> *messages);
bool ValidateImageDetections(const ExpectedFile &expected, int image_index,
                             const std::vector<DetectionEntry> &actual,
                             std::vector<std::string> *messages);
const ExpectedImage *FindExpectedImage(const ExpectedFile &expected, int image_index);

}  // namespace rknn_validation

#endif  // RKNN_YOLOV8_IMAGE_DETECTION_VALIDATION_H_
