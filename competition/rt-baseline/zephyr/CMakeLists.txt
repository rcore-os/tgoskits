# SPDX-License-Identifier: Apache-2.0

cmake_minimum_required(VERSION 3.20.0)
find_package(Zephyr 4.3.0 EXACT REQUIRED HINTS $ENV{ZEPHYR_BASE})
project(zephyr_rt_baseline C)

target_sources(app PRIVATE src/main.c)
target_compile_options(app PRIVATE -Wall -Wextra -Werror)
