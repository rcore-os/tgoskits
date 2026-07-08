cmake_minimum_required(VERSION 3.20)

project(apk_cmake NONE)

if(NOT DEFINED STARRY_STAGING_ROOT)
    message(FATAL_ERROR "STARRY_STAGING_ROOT must be provided by axbuild")
endif()

set(APK_CMAKE_REQUIRED_PROGRAMS
    cmake
    ctest
    cc
    c++
    gcc
    g++
    make
    as
    ld
    ar
    ranlib
)

foreach(program IN LISTS APK_CMAKE_REQUIRED_PROGRAMS)
    find_program(
        APK_CMAKE_${program}_BIN
        NAMES ${program}
        PATHS "${STARRY_STAGING_ROOT}/usr/bin"
        NO_DEFAULT_PATH
    )

    if(NOT APK_CMAKE_${program}_BIN)
        message(FATAL_ERROR "${program} is missing from ${STARRY_STAGING_ROOT}/usr/bin")
    endif()

    install(PROGRAMS "${APK_CMAKE_${program}_BIN}" DESTINATION usr/bin)
endforeach()

set(APK_CMAKE_REQUIRED_DIRS
    lib
    usr/include
    usr/lib
    usr/lib/gcc
    usr/lib/bfd-plugins
    usr/libexec
    usr/libexec/gcc
    usr/share/cmake
)

foreach(rel_dir IN LISTS APK_CMAKE_REQUIRED_DIRS)
    if(EXISTS "${STARRY_STAGING_ROOT}/${rel_dir}")
        install(
            DIRECTORY "${STARRY_STAGING_ROOT}/${rel_dir}/"
            DESTINATION "${rel_dir}"
            USE_SOURCE_PERMISSIONS
        )
    endif()
endforeach()

install(
    PROGRAMS src/apk-cmake.sh
    DESTINATION usr/bin/starry-test-suit
    RENAME apk-cmake
)
