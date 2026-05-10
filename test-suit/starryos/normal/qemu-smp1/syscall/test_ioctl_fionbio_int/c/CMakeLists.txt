cmake_minimum_required(VERSION 3.20)

project(test_ioctl_fionbio_int C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test_ioctl_fionbio_int src/main.c)
target_compile_options(test_ioctl_fionbio_int PRIVATE -Wall -Wextra -Werror)

install(TARGETS test_ioctl_fionbio_int RUNTIME DESTINATION usr/bin)
