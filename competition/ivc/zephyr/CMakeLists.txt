# SPDX-License-Identifier: Apache-2.0

cmake_minimum_required(VERSION 3.20.0)
find_package(Zephyr 4.3.0 REQUIRED HINTS $ENV{ZEPHYR_BASE})
project(ivc_zephyr_endpoint C)

target_sources(app PRIVATE
  src/endpoint.c
  src/main.c
  src/protocol.c
)

target_include_directories(app PRIVATE src)
target_compile_options(app PRIVATE -Wall -Wextra -Werror)
