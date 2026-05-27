cmake_minimum_required(VERSION 3.20)

project(procps-test NONE)

install(PROGRAMS procps-test.sh DESTINATION usr/bin)

set(PROCPS_TOOLS
    ps
    free
    uptime
    pgrep
    pmap
)

foreach(tool IN LISTS PROCPS_TOOLS)
    if(EXISTS "${STARRY_STAGING_ROOT}/usr/bin/${tool}")
        install(PROGRAMS "${STARRY_STAGING_ROOT}/usr/bin/${tool}" DESTINATION usr/bin)
    elseif(EXISTS "${STARRY_STAGING_ROOT}/bin/${tool}")
        install(PROGRAMS "${STARRY_STAGING_ROOT}/bin/${tool}" DESTINATION usr/bin)
    else()
        message(FATAL_ERROR "procps tool ${tool} not found in staging root")
    endif()
endforeach()
