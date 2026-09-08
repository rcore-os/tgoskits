cmake_minimum_required(VERSION 3.20)
project(ltp-syscalls NONE)

set(LTP_SYSCALLS_CASES_FILE "${CMAKE_CURRENT_LIST_DIR}/cases.txt")
file(STRINGS "${LTP_SYSCALLS_CASES_FILE}" LTP_SYSCALLS_CASES
    REGEX "^[A-Za-z0-9_]+$")

if(NOT LTP_SYSCALLS_CASES)
    message(FATAL_ERROR "the frozen PR #1775 LTP manifest is empty")
endif()

# Keep source-derived completion contracts separate from the generated case set.
file(STRINGS "${CMAKE_CURRENT_LIST_DIR}/minimum-passes.txt" LTP_PASS_CONTRACTS)
foreach(LTP_PASS_CONTRACT IN LISTS LTP_PASS_CONTRACTS)
    string(STRIP "${LTP_PASS_CONTRACT}" LTP_PASS_CONTRACT)
    if(LTP_PASS_CONTRACT STREQUAL "" OR LTP_PASS_CONTRACT MATCHES "^#")
        continue()
    endif()
    if(NOT LTP_PASS_CONTRACT MATCHES "^([A-Za-z0-9_]+) +([1-9][0-9]*)$")
        message(FATAL_ERROR "invalid LTP completion contract: ${LTP_PASS_CONTRACT}")
    endif()
    set(LTP_MIN_PASSES_${CMAKE_MATCH_1} "${CMAKE_MATCH_2}")
endforeach()

set(LTP_SYSCALLS_GENERATED_DIR "${CMAKE_CURRENT_BINARY_DIR}/generated")
file(MAKE_DIRECTORY "${LTP_SYSCALLS_GENERATED_DIR}")

foreach(LTP_CASE_ID IN LISTS LTP_SYSCALLS_CASES)
    if(NOT LTP_CASE_ID MATCHES "^[A-Za-z0-9_]+$")
        message(FATAL_ERROR "invalid LTP testcase id: ${LTP_CASE_ID}")
    endif()
    set(LTP_MIN_PASSES 1)
    if(DEFINED LTP_MIN_PASSES_${LTP_CASE_ID})
        set(LTP_MIN_PASSES "${LTP_MIN_PASSES_${LTP_CASE_ID}}")
    endif()
    set(LTP_CASE_WRAPPER
        "${LTP_SYSCALLS_GENERATED_DIR}/ltp-syscalls-${LTP_CASE_ID}")
    configure_file(wrapper.sh.in "${LTP_CASE_WRAPPER}" @ONLY)
    install(PROGRAMS "${LTP_CASE_WRAPPER}"
        DESTINATION usr/bin/starry-test-suit)
endforeach()
