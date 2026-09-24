# Package a matched Alpine executable, loader and C++ runtime as session files.
# The board root filesystem does not need iperf2 or matching library versions.
set(bundle "${CMAKE_CURRENT_BINARY_DIR}/iperf2")
file(MAKE_DIRECTORY "${bundle}")
file(COPY_FILE "${STARRY_STAGING_ROOT}/usr/bin/iperf" "${bundle}/iperf.bin")
file(GLOB loaders "${STARRY_STAGING_ROOT}/lib/ld-musl-*.so.1")
list(LENGTH loaders loader_count)
if(NOT loader_count EQUAL 1)
    message(FATAL_ERROR "Expected one musl loader for the board iperf2 bundle")
endif()
foreach(library IN ITEMS ${loaders}
    "${STARRY_STAGING_ROOT}/usr/lib/libstdc++.so.6"
    "${STARRY_STAGING_ROOT}/usr/lib/libgcc_s.so.1")
    file(COPY "${library}" DESTINATION "${bundle}" FOLLOW_SYMLINK_CHAIN)
endforeach()
file(COPY "${CMAKE_CURRENT_LIST_DIR}/iperf" "${CMAKE_CURRENT_LIST_DIR}/iperf2-smoke"
    DESTINATION "${bundle}" FILE_PERMISSIONS OWNER_READ OWNER_WRITE OWNER_EXECUTE GROUP_READ GROUP_EXECUTE WORLD_READ WORLD_EXECUTE)
file(CHMOD "${bundle}/iperf.bin" PERMISSIONS OWNER_READ OWNER_WRITE OWNER_EXECUTE GROUP_READ GROUP_EXECUTE WORLD_READ WORLD_EXECUTE)
file(COPY "${CMAKE_CURRENT_SOURCE_DIR}/../iperf-smoke.sh" DESTINATION "${bundle}"
    FILE_PERMISSIONS OWNER_READ OWNER_WRITE OWNER_EXECUTE GROUP_READ GROUP_EXECUTE WORLD_READ WORLD_EXECUTE)
file(COPY_FILE "${BOARD_SMOKE_SCRIPT}" "${bundle}/board-smoke.sh")
file(CHMOD "${bundle}/board-smoke.sh" PERMISSIONS OWNER_READ OWNER_WRITE OWNER_EXECUTE GROUP_READ GROUP_EXECUTE WORLD_READ WORLD_EXECUTE)
# Archive only files: the guest creates the destination directory itself,
# so extraction does not need to restore host directory ownership or modes.
file(GLOB bundle_files RELATIVE "${bundle}" "${bundle}/*")
execute_process(COMMAND "${CMAKE_COMMAND}" -E tar czf "${CMAKE_CURRENT_BINARY_DIR}/iperf2.tar.gz"
    "--mtime=1970-01-01 00:00:00 UTC" -- ${bundle_files}
    WORKING_DIRECTORY "${bundle}" COMMAND_ERROR_IS_FATAL ANY)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/iperf2.tar.gz" DESTINATION share)
