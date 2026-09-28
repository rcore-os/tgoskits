#include "detection_validation.h"

#include <stdio.h>
#include <string.h>

using rknn_validation::DetectionEntry;
using rknn_validation::ExpectedFile;
using rknn_validation::ExpectedImage;

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

    printf("PASS detection_validation_selftest\n");
    return 0;
}
