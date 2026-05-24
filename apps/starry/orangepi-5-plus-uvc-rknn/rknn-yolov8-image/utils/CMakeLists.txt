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

add_library(imagedrawing STATIC
    image_drawing.c
)
target_include_directories(imagedrawing PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}
)
