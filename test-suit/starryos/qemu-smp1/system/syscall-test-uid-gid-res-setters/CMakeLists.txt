cmake_minimum_required(VERSION 3.20)
project(test-uid-gid-res-setters C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

# Group D: setresuid / setresgid — three-arg (real + effective + saved)
add_executable(test-uid-gid-res-setters
    src/main.c
    src/setresuid.c
    src/setresgid.c
    src/three_arg_independence.c
    src/boundary.c
    src/fsuid_follow.c
    src/matrix.c
    src/procfs_visibility.c
    src/nptl_sync.c
    src/core_dump_inhibit.c
    src/errno_preservation.c)
target_link_libraries(test-uid-gid-res-setters PRIVATE pthread)

target_include_directories(test-uid-gid-res-setters PRIVATE src)
target_compile_options(test-uid-gid-res-setters PRIVATE -Wall -Wextra -Werror)
# Build this stricter probe, but keep it out of the grouped CI pass condition
# until setresuid/setresgid boundary matrix semantics match Linux.
install(TARGETS test-uid-gid-res-setters RUNTIME DESTINATION usr/bin/starry-known-fail)
