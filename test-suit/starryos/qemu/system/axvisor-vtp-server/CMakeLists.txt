cmake_minimum_required(VERSION 3.20)
project(axvisor-vtp-server C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

# Shared VTP codec lives with the Axvisor E2E test-suit case.
set(VTP_PROTOCOL_DIR "${CMAKE_CURRENT_LIST_DIR}/../../../../axvisor/normal/qemu-vtp/protocol")

starry_arch_filtered_executable(axvisor-vtp-server "aarch64"
    "axvisor-vtp-server skipped on non-aarch64 (Axvisor QEMU aarch64 case)"
    src/main.c ${VTP_PROTOCOL_DIR}/vtp.c)
target_include_directories(axvisor-vtp-server PRIVATE ${VTP_PROTOCOL_DIR})
target_compile_options(axvisor-vtp-server PRIVATE -Wall -Wextra -Werror)
# The grouped system-test runner only globs /usr/bin/starry-test-suit, so this
# binary is installed outside it: it needs a network peer and must not run as a
# standalone unit test.
install(TARGETS axvisor-vtp-server RUNTIME DESTINATION usr/bin)
