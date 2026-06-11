cmake_minimum_required(VERSION 3.20)
project(test-ebpf-basics C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
add_executable(test-ebpf-basics src/main.c)
target_include_directories(test-ebpf-basics PRIVATE src)
target_compile_options(test-ebpf-basics PRIVATE -Wall -Wextra -Werror)
# The default StarryOS CI kernels build starry-kernel with default features
# disabled and do not enable ebpf-kmod, so keep this feature probe out of the
# grouped pass condition until the eBPF-enabled suite is split out.
install(TARGETS test-ebpf-basics RUNTIME DESTINATION usr/bin/starry-known-fail)
