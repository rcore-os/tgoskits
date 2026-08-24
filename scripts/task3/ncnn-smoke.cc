#include <stdint.h>
#include <stdio.h>

struct Task3NcnnDetection {
    uint16_t class_id;
    uint16_t confidence_milli;
    uint16_t center_x_milli;
    uint16_t area_milli;
};

extern "C" int task3_ncnn_infer(const char*, const char*, const char*,
                                 Task3NcnnDetection*, uint64_t*);

int main(int argc, char** argv) {
    const char* param = argc > 1 ? argv[1] : "/usr/share/task3-yolo/yolo11n.ncnn.param";
    const char* model = argc > 2 ? argv[2] : "/usr/share/task3-yolo/yolo11n.ncnn.bin";
    const char* input = argc > 3 ? argv[3] : "/usr/share/task3-yolo/input.ppm";
    Task3NcnnDetection detection{};
    uint64_t infer_us = 0;
    const int status = task3_ncnn_infer(param, model, input, &detection, &infer_us);
    printf("TASK3_NCNN_READY status=%d param=%s model=%s input=%s\n",
           status, param, model, input);
    printf("TASK3_NCNN_INFER infer_us=%llu\n",
           static_cast<unsigned long long>(infer_us));
    if (status == 0) {
        printf("TASK3_NCNN_DETECTION class=%u confidence_milli=%u center_x_milli=%u area_milli=%u\n",
               detection.class_id, detection.confidence_milli,
               detection.center_x_milli, detection.area_milli);
    } else if (status == 1) {
        printf("TASK3_NCNN_DETECTION none\n");
    }
    return status < 0 ? 1 : 0;
}
