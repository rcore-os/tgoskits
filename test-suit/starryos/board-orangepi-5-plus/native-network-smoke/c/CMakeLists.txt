cmake_minimum_required(VERSION 3.21)
project(orangepi_network_smoke C)
set(common "${CMAKE_CURRENT_LIST_DIR}/../../../board-common/network-test")
add_executable(upload-source "${common}/upload-source.c")
target_compile_options(upload-source PRIVATE -Wall -Wextra -Werror)
target_link_options(upload-source PRIVATE -static -no-pie)
install(TARGETS upload-source RUNTIME DESTINATION bin)
install(PROGRAMS "${common}/curl-smoke.sh"
    "${CMAKE_CURRENT_LIST_DIR}/../network-smoke.sh"
    "${CMAKE_CURRENT_LIST_DIR}/../board-smoke.sh" DESTINATION bin)
