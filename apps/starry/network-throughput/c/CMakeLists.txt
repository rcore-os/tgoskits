cmake_minimum_required(VERSION 3.21)
project(starry_network_benchmark C)

add_executable(upload-source
    "${CMAKE_CURRENT_SOURCE_DIR}/../../../../test-suit/starryos/board-common/network-test/upload-source.c")
target_compile_options(upload-source PRIVATE -Wall -Wextra -Werror)
target_link_options(upload-source PRIVATE -static -no-pie)
install(TARGETS upload-source RUNTIME DESTINATION bin)
