cmake_minimum_required(VERSION 3.20)
project(ltp-syscalls NONE)

set(LTP_SYSCALLS_CASES_FILE "${CMAKE_CURRENT_LIST_DIR}/cases.txt")
file(STRINGS "${LTP_SYSCALLS_CASES_FILE}" LTP_SYSCALLS_CASES
    REGEX "^[A-Za-z0-9_]+$")

if(NOT LTP_SYSCALLS_CASES)
    message(FATAL_ERROR "the frozen PR #1775 LTP manifest is empty")
endif()

set(LTP_SYSCALLS_GENERATED_DIR "${CMAKE_CURRENT_BINARY_DIR}/generated")
file(MAKE_DIRECTORY "${LTP_SYSCALLS_GENERATED_DIR}")

foreach(LTP_CASE_ID IN LISTS LTP_SYSCALLS_CASES)
    if(NOT LTP_CASE_ID MATCHES "^[A-Za-z0-9_]+$")
        message(FATAL_ERROR "invalid LTP testcase id: ${LTP_CASE_ID}")
    endif()
    set(LTP_CASE_WRAPPER
        "${LTP_SYSCALLS_GENERATED_DIR}/ltp-syscalls-${LTP_CASE_ID}")
    configure_file(wrapper.sh.in "${LTP_CASE_WRAPPER}" @ONLY)
    install(PROGRAMS "${LTP_CASE_WRAPPER}"
        DESTINATION usr/bin/starry-test-suit)
endforeach()
