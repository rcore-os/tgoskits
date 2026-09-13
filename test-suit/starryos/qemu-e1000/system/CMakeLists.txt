cmake_minimum_required(VERSION 3.20)
project(starry-system-qemu-e1000 C)

include("${CMAKE_CURRENT_LIST_DIR}/../../qemu/system/common/starry_system_test_runner.cmake")
add_subdirectory(test-e1000-napi-runtime)
