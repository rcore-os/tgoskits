cmake_minimum_required(VERSION 3.20)

project(usb-transfer-test C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
set(PKG_CONFIG_USE_CMAKE_PREFIX_PATH OFF)

find_package(PkgConfig REQUIRED)
pkg_check_modules(LIBUSB REQUIRED IMPORTED_TARGET libusb-1.0)

add_executable(usb-transfer-test
    src/main.c
    src/usb_msc_bot.c
)

target_include_directories(usb-transfer-test PRIVATE src)
target_compile_options(usb-transfer-test PRIVATE -Wall -Wextra -Werror)
target_link_libraries(usb-transfer-test PRIVATE PkgConfig::LIBUSB)
