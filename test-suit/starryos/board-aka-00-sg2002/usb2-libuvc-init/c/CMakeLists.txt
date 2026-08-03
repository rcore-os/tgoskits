cmake_minimum_required(VERSION 3.20)
project(sg2002_libuvc_init C)

include(FetchContent)

set(BUILD_EXAMPLE OFF CACHE BOOL "" FORCE)
set(BUILD_TEST OFF CACHE BOOL "" FORCE)
set(CMAKE_BUILD_TARGET Static CACHE STRING "" FORCE)
set(CMAKE_DISABLE_FIND_PACKAGE_JpegPkg TRUE)
set(CMAKE_FIND_LIBRARY_SUFFIXES ".a")

FetchContent_Declare(
    libuvc
    GIT_REPOSITORY https://github.com/libuvc/libuvc.git
    GIT_TAG 68d07a00e11d1944e27b7295ee69673239c00b4b
    GIT_SHALLOW FALSE
)
FetchContent_GetProperties(libuvc)
if(NOT libuvc_POPULATED)
    FetchContent_Populate(libuvc)
endif()
add_subdirectory("${libuvc_SOURCE_DIR}" "${libuvc_BINARY_DIR}" EXCLUDE_FROM_ALL)

add_executable(sg2002-libuvc-init src/main.c)
target_link_libraries(sg2002-libuvc-init PRIVATE LibUVC::UVCStatic)
target_link_options(sg2002-libuvc-init PRIVATE -static)

install(TARGETS sg2002-libuvc-init RUNTIME DESTINATION bin)
