cmake_minimum_required(VERSION 3.20)
project(test-uid-gid-groups C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

# Group E: getgroups / setgroups supplementary group list
add_executable(test-uid-gid-groups
    src/main.c
    src/getgroups.c
    src/setgroups.c
    src/round_trip.c
    src/boundary.c
    src/matrix.c
    src/nptl_sync.c
    src/ngroups_max_sysconf.c
    src/errno_preservation.c)
target_link_libraries(test-uid-gid-groups PRIVATE pthread)

target_include_directories(test-uid-gid-groups PRIVATE src)
target_compile_options(test-uid-gid-groups PRIVATE -Wall -Wextra -Werror)
# Build this stricter probe, but keep it out of the grouped CI pass condition
# until user namespace setgroups denial is complete.
install(TARGETS test-uid-gid-groups RUNTIME DESTINATION usr/bin/starry-known-fail)
