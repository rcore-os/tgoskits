cmake_minimum_required(VERSION 3.20)
project(test-drm-syncobj-lifecycle C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# Keep lifecycle/signal/saturation coverage distinct from the upstream UAPI
# conformance suite, including its installed executable in the grouped runner.
add_executable(test-drm-syncobj-lifecycle src/main.c)
target_compile_options(test-drm-syncobj-lifecycle PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-drm-syncobj-lifecycle RUNTIME DESTINATION usr/bin/starry-test-suit)
