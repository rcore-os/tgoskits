cmake_minimum_required(VERSION 3.20)

project(usbfs-alternate-urb-lifecycle C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
set(PKG_CONFIG_USE_CMAKE_PREFIX_PATH OFF)

if(CMAKE_C_COMPILER_TARGET MATCHES "(x86_64|aarch64|riscv64)")
    find_package(PkgConfig REQUIRED)
    pkg_check_modules(LIBUSB REQUIRED IMPORTED_TARGET libusb-1.0)

    add_executable(usbfs-alternate-urb-lifecycle src/main.c)
    target_link_libraries(usbfs-alternate-urb-lifecycle PRIVATE PkgConfig::LIBUSB)
else()
    starry_arch_filtered_executable(
        usbfs-alternate-urb-lifecycle
        "(x86_64|aarch64|riscv64)"
        "usbfs alternate URB lifecycle test skipped without QEMU xHCI"
        src/main.c
    )
endif()

target_compile_options(usbfs-alternate-urb-lifecycle PRIVATE -Wall -Wextra -Werror)
if(CMAKE_C_COMPILER_TARGET MATCHES "riscv64")
    target_compile_options(usbfs-alternate-urb-lifecycle PRIVATE -march=rv64gc -mabi=lp64d)
endif()
install(TARGETS usbfs-alternate-urb-lifecycle RUNTIME DESTINATION usr/bin/starry-test-suit)
