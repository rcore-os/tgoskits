cmake_minimum_required(VERSION 3.15)

project(rknn_yolov8_image C CXX)

enable_testing()

set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 11)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_INSTALL_RPATH "$ORIGIN/lib")

set(RKNN_IMAGE_ROOT ${CMAKE_CURRENT_SOURCE_DIR})
set(TARGET_SOC rk3588 CACHE STRING "Rockchip target SoC")

add_subdirectory(3rdparty)
add_subdirectory(utils)

add_executable(rknn_yolov8_image
    cpp/main.cc
    cpp/postprocess.cc
    cpp/rknpu2/yolov8.cc
)

add_executable(rknn_yolov8_stream
    cpp/stream_main.cc
    cpp/uvc_capture.cc
    cpp/uvc_capture_layout.cc
    cpp/postprocess.cc
    cpp/rknpu2/yolov8.cc
)

add_executable(rknn_yolov8_bench
    cpp/bench_main.cc
    cpp/detection_validation.cc
    cpp/uvc_capture.cc
    cpp/uvc_capture_layout.cc
    cpp/postprocess.cc
    cpp/rknpu2/yolov8.cc
)

add_executable(uvc_capture_layout_selftest
    cpp/uvc_capture_layout.cc
    cpp/uvc_capture_layout_selftest.cc
)

add_executable(uvc_capture_mjpeg_selftest
    cpp/uvc_capture.cc
    cpp/uvc_capture_layout.cc
    cpp/uvc_capture_mjpeg_selftest.cc
)

target_include_directories(rknn_yolov8_image PRIVATE
    ${RKNN_IMAGE_ROOT}/cpp
    ${RKNN_IMAGE_ROOT}/utils
    ${LIBRKNNRT_INCLUDES}
    ${LIBJPEG_INCLUDES}
)

target_include_directories(rknn_yolov8_stream PRIVATE
    ${RKNN_IMAGE_ROOT}/cpp
    ${RKNN_IMAGE_ROOT}/utils
    ${LIBRKNNRT_INCLUDES}
    ${LIBJPEG_INCLUDES}
)

target_include_directories(rknn_yolov8_bench PRIVATE
    ${RKNN_IMAGE_ROOT}/cpp
    ${RKNN_IMAGE_ROOT}/utils
    ${LIBRKNNRT_INCLUDES}
    ${LIBJPEG_INCLUDES}
)

target_include_directories(uvc_capture_layout_selftest PRIVATE
    ${RKNN_IMAGE_ROOT}/cpp
    ${RKNN_IMAGE_ROOT}/utils
)

target_include_directories(uvc_capture_mjpeg_selftest PRIVATE
    ${RKNN_IMAGE_ROOT}/cpp
    ${RKNN_IMAGE_ROOT}/utils
    ${LIBJPEG_INCLUDES}
)

if(CMAKE_CXX_COMPILER_ID MATCHES "GNU|Clang")
    target_compile_options(uvc_capture_mjpeg_selftest PRIVATE -ffunction-sections -fdata-sections)
    if(APPLE)
        target_link_options(uvc_capture_mjpeg_selftest PRIVATE -Wl,-dead_strip)
    else()
        target_link_options(uvc_capture_mjpeg_selftest PRIVATE -Wl,--gc-sections)
    endif()
endif()

add_test(NAME uvc_capture_layout_selftest COMMAND uvc_capture_layout_selftest)
add_test(NAME uvc_capture_mjpeg_selftest COMMAND uvc_capture_mjpeg_selftest)

target_link_libraries(rknn_yolov8_image
    imageutils
    fileutils
    ${LIBRKNNRT}
    dl
)

target_link_libraries(rknn_yolov8_stream
    imageutils
    fileutils
    imagedrawing
    ${LIBRKNNRT}
    ${LIBJPEG}
    dl
)

target_link_libraries(rknn_yolov8_bench
    imageutils
    fileutils
    ${LIBRKNNRT}
    ${LIBJPEG}
    dl
)

if (CMAKE_SYSTEM_NAME STREQUAL "Linux")
    set(THREADS_PREFER_PTHREAD_FLAG ON)
    find_package(Threads REQUIRED)
    target_link_libraries(rknn_yolov8_image Threads::Threads)
    target_link_libraries(rknn_yolov8_stream Threads::Threads)
    target_link_libraries(rknn_yolov8_bench Threads::Threads)
    target_link_libraries(uvc_capture_mjpeg_selftest PRIVATE dl)
endif()

install(TARGETS rknn_yolov8_image DESTINATION .)
install(TARGETS rknn_yolov8_stream DESTINATION .)
install(TARGETS rknn_yolov8_bench DESTINATION .)
install(DIRECTORY model DESTINATION .)
install(DIRECTORY validation DESTINATION . OPTIONAL)
