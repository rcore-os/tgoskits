cmake_minimum_required(VERSION 3.20.0)

find_package(Zephyr REQUIRED HINTS $ENV{ZEPHYR_BASE})
project(periodic_latency)

target_sources(app PRIVATE src/main.c)

if(RT_START_GATED)
  target_compile_definitions(app PRIVATE RT_START_GATED=1)
endif()

if(RT_DUMP_GATED)
  target_compile_definitions(app PRIVATE RT_DUMP_GATED=1)
endif()

if(DEFINED RT_SAMPLE_COUNT)
  target_compile_definitions(app PRIVATE RT_SAMPLE_COUNT=${RT_SAMPLE_COUNT})
endif()

target_compile_definitions(app PRIVATE RT_START_DELAY_MS=${RT_START_DELAY_MS})
