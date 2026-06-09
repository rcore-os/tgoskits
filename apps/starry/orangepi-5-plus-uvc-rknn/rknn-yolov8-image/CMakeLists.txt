cmake_minimum_required(VERSION 3.15)

project(rknn_yolov8_image C CXX)

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
    cpp/postprocess.cc
    cpp/rknpu2/yolov8.cc
)

add_executable(rknn_yolov8_bench
    cpp/bench_main.cc
    cpp/uvc_capture.cc
    cpp/postprocess.cc
    cpp/rknpu2/yolov8.cc
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
endif()

install(TARGETS rknn_yolov8_image DESTINATION .)
install(TARGETS rknn_yolov8_stream DESTINATION .)
install(TARGETS rknn_yolov8_bench DESTINATION .)
install(DIRECTORY model DESTINATION .)
