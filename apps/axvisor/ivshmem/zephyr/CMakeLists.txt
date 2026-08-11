cmake_minimum_required(VERSION 3.20.0)
find_package(Zephyr REQUIRED HINTS $ENV{ZEPHYR_BASE})
project(axvisor_ivshmem_sdk_peer)

if(EXISTS "${CMAKE_CURRENT_LIST_DIR}/common/include/ivc_sdk.h")
  set(IVC_COMMON_DIR "${CMAKE_CURRENT_LIST_DIR}/common")
else()
  set(IVC_COMMON_DIR "${CMAKE_CURRENT_LIST_DIR}/../common")
endif()

target_sources(app PRIVATE
  src/main.cpp
  src/ivc_platform.c
  ${IVC_COMMON_DIR}/src/ivc_ring.c
  ${IVC_COMMON_DIR}/src/ivc_client.c
  ${IVC_COMMON_DIR}/src/ivc_demo.c
  ${IVC_COMMON_DIR}/src/ivc_sdk.c
)
target_include_directories(app PRIVATE ${IVC_COMMON_DIR}/include)
