cmake_minimum_required(VERSION 3.20)
project(perf-validate C)

# Board session-asset build for the RK3588 perf-validate validator.
#
# `cargo xtask starry test board` cross-compiles this project with the shared
# musl toolchain on EVERY run, then uploads the CMake `install()` products as
# session files (see test-suit/starryos/GUIDE.md). The board `shell_init_cmd`
# downloads `${sessionFile:bin/perf-validate}`, chmods it, and executes it — so
# the binary the board runs is always built from THIS commit's source, with no
# reliance on a manually pre-staged persistent-rootfs binary.
#
# The board is always aarch64, so (unlike the QEMU system case, which must build
# a skip stub on non-aarch64 hosts) no arch filter is needed here. `-static`
# keeps the ELF self-contained: StarryOS loads static ELFs and the same file can
# be sanity-checked under the board's OrangePi Linux on the shared rootfs.
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(perf-validate src/perf_validate.c)
target_compile_options(perf-validate PRIVATE -Wall -Wextra -Werror)
target_link_options(perf-validate PRIVATE -static)

# Uploaded and downloaded as `${sessionFile:bin/perf-validate}`.
install(TARGETS perf-validate RUNTIME DESTINATION bin)
