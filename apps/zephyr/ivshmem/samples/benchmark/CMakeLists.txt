cmake_minimum_required(VERSION 3.20.0)

set(KCONFIG_ROOT ${CMAKE_CURRENT_SOURCE_DIR}/../common/Kconfig)

set(AXIVC_BENCH_PREPARED "$ENV{AXIVC_BENCH_PREPARED}")
if("${AXIVC_BENCH_PREPARED}" STREQUAL "")
  set(AXIVC_BENCH_PREPARED 0)
endif()
if(NOT "${AXIVC_BENCH_PREPARED}" MATCHES "^(0|1)$")
  message(FATAL_ERROR "AXIVC_BENCH_PREPARED must be 0 or 1")
endif()
if("${AXIVC_BENCH_PREPARED}" STREQUAL "1")
  list(APPEND EXTRA_CONF_FILE
    "${CMAKE_CURRENT_SOURCE_DIR}/prepared.conf")
endif()

find_package(Zephyr REQUIRED HINTS $ENV{ZEPHYR_BASE})
project(tgos_ivc_starry_benchmark)

target_include_directories(app PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/../common)
target_sources(app PRIVATE src/main.c)
target_compile_definitions(
  app PRIVATE AXIVC_BENCH_PREPARED=${AXIVC_BENCH_PREPARED})

if(DEFINED ENV{AXIVC_BENCH_ITERATIONS} AND
   NOT "$ENV{AXIVC_BENCH_ITERATIONS}" STREQUAL "")
  if(NOT "$ENV{AXIVC_BENCH_ITERATIONS}" MATCHES "^[1-9][0-9]*$")
    message(FATAL_ERROR "AXIVC_BENCH_ITERATIONS must be a positive integer")
  endif()
  target_compile_definitions(
    app PRIVATE AXIVC_BENCH_ITERATIONS=$ENV{AXIVC_BENCH_ITERATIONS})
endif()
