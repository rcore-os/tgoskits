cmake_minimum_required(VERSION 3.20)
project(docker-guest-env C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(docker-guest-env-probe src/main.c)
target_compile_options(docker-guest-env-probe PRIVATE -Wall -Wextra -Werror)
# The probe is cross-built against the Alpine musl toolchain sysroot and runs
# on the Debian glibc rootfs, so it must not carry runtime library deps.
target_link_options(docker-guest-env-probe PRIVATE -static)

install(TARGETS docker-guest-env-probe RUNTIME DESTINATION usr/bin)
install(PROGRAMS docker-guest-env-smoke.sh DESTINATION usr/bin)
