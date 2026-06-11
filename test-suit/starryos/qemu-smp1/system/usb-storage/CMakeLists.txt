cmake_minimum_required(VERSION 3.20)

project(usb-storage-test C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
set(PKG_CONFIG_USE_CMAKE_PREFIX_PATH OFF)

if(CMAKE_C_COMPILER_TARGET MATCHES "(x86_64|aarch64|riscv64)")
    if(NOT DEFINED STARRY_STAGING_ROOT)
        message(FATAL_ERROR "STARRY_STAGING_ROOT must be provided by axbuild")
    endif()

    file(GLOB LIBUSB_RUNTIME_CANDIDATES
        "${STARRY_STAGING_ROOT}/usr/lib/libusb-1.0.so.0*"
    )
    if(NOT LIBUSB_RUNTIME_CANDIDATES)
        message(FATAL_ERROR "libusb runtime is missing from ${STARRY_STAGING_ROOT}/usr/lib")
    endif()
    list(SORT LIBUSB_RUNTIME_CANDIDATES)
    list(REVERSE LIBUSB_RUNTIME_CANDIDATES)
    list(GET LIBUSB_RUNTIME_CANDIDATES 0 LIBUSB_RUNTIME)

    find_package(PkgConfig REQUIRED)
    pkg_check_modules(LIBUSB REQUIRED IMPORTED_TARGET libusb-1.0)

    add_executable(usb-storage-test
        src/main.c
        src/usb_msc_bot.c
    )
    target_include_directories(usb-storage-test PRIVATE src)
    target_link_libraries(usb-storage-test PRIVATE PkgConfig::LIBUSB)
    install(FILES "${LIBUSB_RUNTIME}" DESTINATION usr/lib RENAME libusb-1.0.so.0)
else()
    starry_arch_filtered_executable(
        usb-storage-test
        "(x86_64|aarch64|riscv64)"
        "USB storage tests skipped on loongarch64"
        src/main.c
    )
endif()

target_compile_options(usb-storage-test PRIVATE -Wall -Wextra -Werror)
install(TARGETS usb-storage-test RUNTIME DESTINATION usr/bin/starry-test-suit)
