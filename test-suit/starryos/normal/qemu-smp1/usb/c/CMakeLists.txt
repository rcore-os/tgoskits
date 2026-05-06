cmake_minimum_required(VERSION 3.20)

project(usb-transfer-test C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
set(PKG_CONFIG_USE_CMAKE_PREFIX_PATH OFF)

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

set(LSUSB_RUNTIME "${STARRY_STAGING_ROOT}/usr/bin/lsusb")
if(NOT EXISTS "${LSUSB_RUNTIME}")
    message(FATAL_ERROR "lsusb runtime is missing from ${STARRY_STAGING_ROOT}/usr/bin")
endif()

set(UDEV_HWDB "${STARRY_STAGING_ROOT}/usr/lib/udev/hwdb.bin")
if(NOT EXISTS "${UDEV_HWDB}")
    message(FATAL_ERROR "udev hardware database is missing from ${STARRY_STAGING_ROOT}/usr/lib/udev")
endif()

find_package(PkgConfig REQUIRED)
pkg_check_modules(LIBUSB REQUIRED IMPORTED_TARGET libusb-1.0)

add_executable(usb-transfer-test
    src/main.c
    src/usb_msc_bot.c
)

target_include_directories(usb-transfer-test PRIVATE src)
target_compile_options(usb-transfer-test PRIVATE -Wall -Wextra -Werror)
target_link_libraries(usb-transfer-test PRIVATE PkgConfig::LIBUSB)

install(TARGETS usb-transfer-test RUNTIME DESTINATION usr/bin)
install(PROGRAMS "${LSUSB_RUNTIME}" DESTINATION usr/bin)
install(FILES "${LIBUSB_RUNTIME}" DESTINATION usr/lib RENAME libusb-1.0.so.0)

if(EXISTS "${STARRY_STAGING_ROOT}/usr/share/hwdata/usb.ids")
    install(FILES "${STARRY_STAGING_ROOT}/usr/share/hwdata/usb.ids" DESTINATION usr/share/hwdata)
endif()
install(FILES "${UDEV_HWDB}" DESTINATION usr/lib/udev)
