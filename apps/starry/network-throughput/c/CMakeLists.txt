cmake_minimum_required(VERSION 3.21)
project(starry_network_benchmark C)
set(common "${CMAKE_CURRENT_SOURCE_DIR}/../../../../test-suit/starryos/board-common/network-test")
include("${common}/curl-bundle.cmake")
install_starry_curl_bundle()

add_executable(upload-source
    "${common}/upload-source.c")
target_compile_options(upload-source PRIVATE -Wall -Wextra -Werror)
target_link_options(upload-source PRIVATE -static -no-pie)
install(TARGETS upload-source RUNTIME DESTINATION bin)
