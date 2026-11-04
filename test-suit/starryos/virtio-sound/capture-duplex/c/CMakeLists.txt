cmake_minimum_required(VERSION 3.20)
project(virtio-sound-capture C)

include("${CMAKE_CURRENT_LIST_DIR}/../../common/c/common.cmake")
starry_virtio_sound_case(virtio-sound-capture src/main.c)
