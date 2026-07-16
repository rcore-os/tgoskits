cmake_minimum_required(VERSION 3.20)
project(test-user-register-roundtrip C ASM)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

starry_arch_filtered_executable(test-user-register-roundtrip "riscv64|loongarch64" "test-user-register-roundtrip skipped on unsupported architecture" src/main.c src/register_signal_handler.S)
target_compile_options(test-user-register-roundtrip PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-user-register-roundtrip RUNTIME DESTINATION usr/bin/starry-test-suit)
