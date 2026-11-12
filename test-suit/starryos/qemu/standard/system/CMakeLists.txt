cmake_minimum_required(VERSION 3.20)
project(starry-system-qemu C)

include("${CMAKE_CURRENT_LIST_DIR}/../../../common/cmake/starry_system_test_runner.cmake")

file(GLOB_RECURSE STARRY_SYSTEM_PROJECTS
    RELATIVE "${CMAKE_CURRENT_LIST_DIR}"
    CONFIGURE_DEPENDS "*/CMakeLists.txt"
)

# Project IDs remain independent of the functional category directory.
set(STARRY_SYSTEM_SUBCASES)
foreach(STARRY_SYSTEM_PROJECT IN LISTS STARRY_SYSTEM_PROJECTS)
    get_filename_component(STARRY_SYSTEM_PROJECT_DIR "${STARRY_SYSTEM_PROJECT}" DIRECTORY)
    if(STARRY_SYSTEM_PROJECT_DIR STREQUAL "" OR STARRY_SYSTEM_PROJECT_DIR STREQUAL ".")
        continue()
    endif()
    # A project's own dependencies belong to that project, not to the suite.
    get_filename_component(STARRY_SYSTEM_PARENT "${STARRY_SYSTEM_PROJECT_DIR}" DIRECTORY)
    set(STARRY_SYSTEM_NESTED FALSE)
    while(NOT STARRY_SYSTEM_PARENT STREQUAL "" AND NOT STARRY_SYSTEM_PARENT STREQUAL ".")
        if(EXISTS "${CMAKE_CURRENT_LIST_DIR}/${STARRY_SYSTEM_PARENT}/CMakeLists.txt")
            set(STARRY_SYSTEM_NESTED TRUE)
            break()
        endif()
        get_filename_component(STARRY_SYSTEM_PARENT "${STARRY_SYSTEM_PARENT}" DIRECTORY)
    endwhile()
    if(STARRY_SYSTEM_NESTED)
        continue()
    endif()
    get_filename_component(STARRY_SYSTEM_PROJECT_ID "${STARRY_SYSTEM_PROJECT_DIR}" NAME)
    if(STARRY_SYSTEM_PROJECT_ID IN_LIST STARRY_SYSTEM_SUBCASES)
        message(FATAL_ERROR "duplicate system project ID: ${STARRY_SYSTEM_PROJECT_ID}")
    endif()
    list(APPEND STARRY_SYSTEM_SUBCASES "${STARRY_SYSTEM_PROJECT_ID}")
    set("STARRY_SYSTEM_SOURCE_${STARRY_SYSTEM_PROJECT_ID}" "${STARRY_SYSTEM_PROJECT_DIR}")
endforeach()
list(SORT STARRY_SYSTEM_SUBCASES)

if(DEFINED STARRY_GROUPED_C_SUBCASES)
    set(STARRY_SYSTEM_SELECTED_SUBCASES ${STARRY_GROUPED_C_SUBCASES})
else()
    set(STARRY_SYSTEM_SELECTED_SUBCASES ${STARRY_SYSTEM_SUBCASES})
endif()

foreach(STARRY_SYSTEM_SUBCASE IN LISTS STARRY_SYSTEM_SELECTED_SUBCASES)
    if(NOT STARRY_SYSTEM_SUBCASE IN_LIST STARRY_SYSTEM_SUBCASES)
        message(FATAL_ERROR "unknown grouped C subcase: ${STARRY_SYSTEM_SUBCASE}")
    endif()
    add_subdirectory("${STARRY_SYSTEM_SOURCE_${STARRY_SYSTEM_SUBCASE}}"
                     "${STARRY_SYSTEM_SUBCASE}")
endforeach()
