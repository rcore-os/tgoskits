#include "detection_validation.h"

#include <stdio.h>
#include <string.h>

using rknn_validation::DetectionEntry;
using rknn_validation::ExpectedFile;
using rknn_validation::ExpectedImage;
using rknn_validation::SortingAction;

static int require_true(bool value, const char *message)
{
    if (!value) {
        printf("FAIL: %s\n", message);
        return 1;
    }
    return 0;
}

int main()
{
    ExpectedImage expected_image;
    expected_image.index = 0;
    expected_image.path = "validation/tennis-ball-close.jpg";
    expected_image.width = 1535;
    expected_image.height = 2048;
    expected_image.detections.push_back(DetectionEntry(32, 8732, 510, 930, 760, 1240));

    ExpectedFile expected;
    expected.min_confidence = 25;
    expected.nms_threshold_q10000 = 4500;
    expected.images.push_back(expected_image);

    const std::string path = "/tmp/rknn_validate_expected_selftest.txt";
    std::string error;
    if (require_true(rknn_validation::WriteExpectedFile(path, expected, &error), error.c_str()) != 0) {
        return 1;
    }

    ExpectedFile parsed;
    if (require_true(rknn_validation::ReadExpectedFile(path, &parsed, &error), error.c_str()) != 0) {
        return 1;
    }
    if (require_true(parsed.images.size() == 1, "parsed one image") != 0) {
        return 1;
    }
    if (require_true(parsed.images[0].detections.size() == 1, "parsed one detection") != 0) {
        return 1;
    }

    std::vector<rknn_validation::ValidationImage> images;
    if (require_true(rknn_validation::ParseImageList(
                         "# validation images\nvalidation/tennis-ball-close.jpg\nvalidation/tennis-ball-plant.jpg\n",
                         &images,
                         &error),
                     "image list parses") != 0) {
        return 1;
    }
    if (require_true(images.size() == 2 && images[1].index == 1, "image list assigns sequential indexes") != 0) {
        return 1;
    }

    ExpectedFile bad_expected;
    if (require_true(!rknn_validation::ParseExpectedFile(
                         "RKNN_VALIDATE_EXPECTED version=1 image_count=1 min_confidence=25 nms_threshold_q10000=4500\n"
                         "image index=0 path=validation/tennis-ball-close.jpg width=1535 height=2048 count=1\n",
                         &bad_expected,
                         &error),
                     "declared detection count mismatch fails") != 0) {
        return 1;
    }

    std::vector<DetectionEntry> actual;
    actual.push_back(DetectionEntry(32, 7600, 515, 935, 755, 1235));
    std::vector<std::string> messages;
    if (require_true(rknn_validation::ValidateDetections(parsed.images[0], actual, &messages), "nearby detection matches") != 0) {
        return 1;
    }

    actual[0].cls_id = 0;
    if (require_true(!rknn_validation::ValidateDetections(parsed.images[0], actual, &messages), "class mismatch fails") != 0) {
        return 1;
    }
    if (require_true(!messages.empty() && strstr(messages[0].c_str(), "cls") != NULL, "class mismatch explains cls") != 0) {
        return 1;
    }

    actual[0] = DetectionEntry(32, 7600, 0, 0, 10, 10);
    if (require_true(!rknn_validation::ValidateDetections(parsed.images[0], actual, &messages), "low IoU fails") != 0) {
        return 1;
    }

    actual[0] = DetectionEntry(32, 1000, 515, 935, 755, 1235);
    if (require_true(!rknn_validation::ValidateDetections(parsed.images[0], actual, &messages), "score delta fails") != 0) {
        return 1;
    }

    actual.clear();
    actual.push_back(DetectionEntry(32, 7081, 479, 706, 773, 1010));
    rknn_validation::SortingDecision sorting =
        rknn_validation::SelectSortingDecision(actual, 32, 625);
    if (require_true(sorting.detection_present && sorting.action == SortingAction::SortRight &&
                         sorting.detection.left == 479 && sorting.detection.right == 773,
                     "sports ball right of calibration selects right") != 0) {
        return 1;
    }
    actual.push_back(DetectionEntry(32, 8591, 517, 932, 730, 1151));
    sorting = rknn_validation::SelectSortingDecision(actual, 32, 625);
    if (require_true(sorting.action == SortingAction::SortLeft && sorting.detection.score_q10000 == 8591,
                     "highest-confidence sports ball selects left") != 0) {
        return 1;
    }
    actual.clear();
    actual.push_back(DetectionEntry(58, 8159, 868, 335, 1167, 538));
    sorting = rknn_validation::SelectSortingDecision(actual, 32, 625);
    if (require_true(!sorting.detection_present && sorting.action == SortingAction::Hold,
                     "missing target selects hold") != 0) {
        return 1;
    }
    const std::string hold_record = rknn_validation::FormatVisionDecisionRecord(
        42, 1000, 1500, 1000000, sorting);
    if (require_true(
            hold_record ==
                "VISION_DECISION_RECORD version=1 frame_id=42 captured_at_us=1000 "
                "inference_finished_at_us=1500 ttl_us=1000000 requested_action=hold "
                "safe_action=hold detection_present=0 class_id=65535 confidence_q10000=0 "
                "region_id=0 left=0 top=0 right=0 bottom=0",
            "hold decision record preserves one frame identity") != 0) {
        return 1;
    }

    actual.clear();
    actual.push_back(DetectionEntry(32, 7081, 170, 70, 230, 130));
    sorting = rknn_validation::SelectSortingDecision(actual, 32, 160);
    const std::string right_record = rknn_validation::FormatVisionDecisionRecord(
        43, 2000, 2600, 750000, sorting);
    if (require_true(
            right_record.find("frame_id=43") != std::string::npos &&
                right_record.find("requested_action=right") != std::string::npos &&
                right_record.find("left=170 top=70 right=230 bottom=130") != std::string::npos,
            "right decision record carries the selected bounding box") != 0) {
        return 1;
    }

    printf("PASS detection_validation_selftest\n");
    return 0;
}
