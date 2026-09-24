cmake_minimum_required(VERSION 3.15)

project(rknn_yolov8_image_utils)

add_library(fileutils STATIC
    file_utils.c
)
target_include_directories(fileutils PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}
)

add_definitions(-DLIBRGA_IM2D_HANDLE)

add_library(imageutils STATIC
    image_utils.c
)
target_include_directories(imageutils PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}
    ${STB_INCLUDES}
    ${LIBRGA_INCLUDES}
    ${LIBJPEG_INCLUDES}
)
target_link_libraries(imageutils
    ${LIBRGA}
    ${LIBJPEG}
)

add_executable(image_utils_jpeg_selftest
    image_utils.c
    image_utils_jpeg_selftest.c
)
target_include_directories(image_utils_jpeg_selftest PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}
    ${STB_INCLUDES}
    ${LIBRGA_INCLUDES}
    ${LIBJPEG_INCLUDES}
)
target_link_libraries(image_utils_jpeg_selftest PRIVATE m)
if(CMAKE_C_COMPILER_ID MATCHES "GNU|Clang")
    target_compile_options(image_utils_jpeg_selftest PRIVATE -ffunction-sections -fdata-sections)
    if(APPLE)
        target_link_options(image_utils_jpeg_selftest PRIVATE -Wl,-dead_strip)
    else()
        target_link_options(image_utils_jpeg_selftest PRIVATE -Wl,--gc-sections)
    endif()
endif()
add_test(NAME image_utils_jpeg_selftest COMMAND image_utils_jpeg_selftest)

add_library(imagedrawing STATIC
    image_drawing.c
)
target_include_directories(imagedrawing PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}
)
