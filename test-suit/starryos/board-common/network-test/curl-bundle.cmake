function(install_starry_curl_bundle)
    set(curl_binary "${STARRY_STAGING_ROOT}/usr/bin/curl")
    if(NOT EXISTS "${curl_binary}")
        message(FATAL_ERROR "curl is missing from the staging root; run c/prebuild.sh")
    endif()

    set(bundle "${CMAKE_CURRENT_BINARY_DIR}/curl-bundle")
    file(REMOVE_RECURSE "${bundle}")
    file(MAKE_DIRECTORY "${bundle}")

    # Resolve the target ELF closure, then reject dependencies outside the staging root.
    set(CMAKE_GET_RUNTIME_DEPENDENCIES_PLATFORM linux+elf)
    set(CMAKE_GET_RUNTIME_DEPENDENCIES_TOOL objdump)
    set(CMAKE_GET_RUNTIME_DEPENDENCIES_COMMAND "${CMAKE_OBJDUMP}")
    file(GET_RUNTIME_DEPENDENCIES
        EXECUTABLES "${curl_binary}"
        DIRECTORIES "${STARRY_STAGING_ROOT}/lib" "${STARRY_STAGING_ROOT}/usr/lib"
        RESOLVED_DEPENDENCIES_VAR libraries
        UNRESOLVED_DEPENDENCIES_VAR unresolved)
    if(unresolved)
        message(FATAL_ERROR "Unresolved curl libraries: ${unresolved}")
    endif()
    file(GLOB loaders "${STARRY_STAGING_ROOT}/lib/ld-musl-*.so.1")
    list(LENGTH loaders loader_count)
    if(NOT loader_count EQUAL 1)
        message(FATAL_ERROR "Expected one musl loader for the curl bundle")
    endif()
    list(APPEND libraries ${loaders})
    foreach(library IN LISTS libraries)
        cmake_path(IS_PREFIX STARRY_STAGING_ROOT "${library}" NORMALIZE in_staging)
        if(NOT in_staging)
            message(FATAL_ERROR "curl dependency escaped staging root: ${library}")
        endif()
        file(COPY "${library}" DESTINATION "${bundle}" FOLLOW_SYMLINK_CHAIN)
    endforeach()
    file(COPY_FILE "${curl_binary}" "${bundle}/curl.bin")
    file(COPY "${CMAKE_CURRENT_FUNCTION_LIST_DIR}/curl" DESTINATION "${bundle}"
        FILE_PERMISSIONS OWNER_READ OWNER_WRITE OWNER_EXECUTE GROUP_READ GROUP_EXECUTE WORLD_READ WORLD_EXECUTE)
    file(CHMOD "${bundle}/curl.bin" PERMISSIONS OWNER_READ OWNER_WRITE OWNER_EXECUTE GROUP_READ GROUP_EXECUTE WORLD_READ WORLD_EXECUTE)

    set(archive "${CMAKE_CURRENT_BINARY_DIR}/curl-bundle.tar.gz")
    add_custom_command(OUTPUT "${archive}"
        COMMAND "${CMAKE_COMMAND}" -E tar czf "${archive}"
            "--mtime=1970-01-01 00:00:00 UTC" -- .
        WORKING_DIRECTORY "${bundle}"
        DEPENDS "${curl_binary}" "${CMAKE_CURRENT_FUNCTION_LIST_DIR}/curl")
    add_custom_target(starry-curl-bundle ALL DEPENDS "${archive}")
    install(FILES "${archive}" DESTINATION share)
endfunction()
