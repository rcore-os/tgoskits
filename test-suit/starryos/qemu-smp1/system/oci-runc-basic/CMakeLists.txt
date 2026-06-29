cmake_minimum_required(VERSION 3.20)
project(oci-runc-basic NONE)

install(
    PROGRAMS run.sh
    DESTINATION usr/bin/starry-test-suit
    RENAME oci-runc-basic
)

if(CMAKE_SYSTEM_PROCESSOR STREQUAL "x86_64")
    set(STARRY_RUNC "${STARRY_STAGING_ROOT}/usr/bin/runc")
    if(NOT EXISTS "${STARRY_RUNC}")
        message(FATAL_ERROR "runc is missing from the staging root")
    endif()
    install(PROGRAMS "${STARRY_RUNC}" DESTINATION usr/bin)

    # runc depends on libseccomp for seccomp syscall filtering.
    set(STARRY_LIBSECCOMP "${STARRY_STAGING_ROOT}/usr/lib/libseccomp.so.2")
    if(EXISTS "${STARRY_LIBSECCOMP}")
        install(PROGRAMS "${STARRY_LIBSECCOMP}" DESTINATION usr/lib)
    endif()
    set(STARRY_LIBSECCOMP_FULL "${STARRY_LIBSECCOMP}.6.0")
    if(EXISTS "${STARRY_LIBSECCOMP_FULL}")
        install(PROGRAMS "${STARRY_LIBSECCOMP_FULL}" DESTINATION usr/lib)
    endif()
endif()
