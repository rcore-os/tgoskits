cmake_minimum_required(VERSION 3.21)
project(sysbench_harness C)

set(harness "${CMAKE_CURRENT_SOURCE_DIR}/../harness")
set(bundle "${CMAKE_CURRENT_BINARY_DIR}/sysbench")
file(REMOVE_RECURSE "${bundle}")
file(MAKE_DIRECTORY "${bundle}")
foreach(probe IN ITEMS cpuprobe membw)
    add_executable(${probe} "${harness}/${probe}.c")
    target_compile_options(${probe} PRIVATE -Wall -Wextra -Werror)
    target_link_options(${probe} PRIVATE -static -no-pie)
    set_target_properties(${probe} PROPERTIES RUNTIME_OUTPUT_DIRECTORY "${bundle}")
endforeach()

# Resolve the actual ELF closure; reject accidental host-library resolution.
set(CMAKE_GET_RUNTIME_DEPENDENCIES_PLATFORM linux+elf)
set(CMAKE_GET_RUNTIME_DEPENDENCIES_TOOL objdump)
set(CMAKE_GET_RUNTIME_DEPENDENCIES_COMMAND "${CMAKE_OBJDUMP}")
file(GET_RUNTIME_DEPENDENCIES
    EXECUTABLES "${STARRY_STAGING_ROOT}/usr/bin/sysbench"
    DIRECTORIES "${STARRY_STAGING_ROOT}/lib" "${STARRY_STAGING_ROOT}/usr/lib"
    RESOLVED_DEPENDENCIES_VAR libraries
    UNRESOLVED_DEPENDENCIES_VAR unresolved)
if(unresolved)
    message(FATAL_ERROR "Unresolved sysbench libraries: ${unresolved}")
endif()
file(GLOB loaders "${STARRY_STAGING_ROOT}/lib/ld-musl-*.so.1")
list(APPEND libraries ${loaders})
foreach(library IN LISTS libraries)
    cmake_path(IS_PREFIX STARRY_STAGING_ROOT "${library}" NORMALIZE in_staging)
    if(NOT in_staging)
        message(FATAL_ERROR "sysbench dependency escaped staging root: ${library}")
    endif()
    file(COPY "${library}" DESTINATION "${bundle}" FOLLOW_SYMLINK_CHAIN)
endforeach()
file(COPY_FILE "${STARRY_STAGING_ROOT}/usr/bin/sysbench" "${bundle}/sysbench.bin")
file(COPY "${harness}/sysbench" "${harness}/run.sh" DESTINATION "${bundle}"
    FILE_PERMISSIONS OWNER_READ OWNER_WRITE OWNER_EXECUTE GROUP_READ GROUP_EXECUTE WORLD_READ WORLD_EXECUTE)
file(CHMOD "${bundle}/sysbench.bin" PERMISSIONS OWNER_READ OWNER_WRITE OWNER_EXECUTE GROUP_READ GROUP_EXECUTE WORLD_READ WORLD_EXECUTE)
find_program(TAR_EXECUTABLE tar REQUIRED)
find_program(GZIP_EXECUTABLE gzip REQUIRED)
add_custom_command(OUTPUT "${CMAKE_CURRENT_BINARY_DIR}/sysbench.tar.gz"
    COMMAND "${TAR_EXECUTABLE}" --sort=name --mtime=@0 --owner=0 --group=0
        --numeric-owner --mode=u+rwX,go+rX,go-w
        -cf "${CMAKE_CURRENT_BINARY_DIR}/sysbench.tar" -- .
    COMMAND "${GZIP_EXECUTABLE}" -n -f "${CMAKE_CURRENT_BINARY_DIR}/sysbench.tar"
    WORKING_DIRECTORY "${bundle}"
    DEPENDS cpuprobe membw "${harness}/run.sh" "${harness}/sysbench")
add_custom_target(bundle ALL DEPENDS "${CMAKE_CURRENT_BINARY_DIR}/sysbench.tar.gz")
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/sysbench.tar.gz" DESTINATION share)
