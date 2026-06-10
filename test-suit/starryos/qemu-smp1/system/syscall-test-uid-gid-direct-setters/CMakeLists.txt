cmake_minimum_required(VERSION 3.20)
project(test-uid-gid-direct-setters C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

# Group B: setuid / setgid 直接 setter syscall 地毯式覆盖。
add_executable(test-uid-gid-direct-setters
    src/main.c
    src/setuid.c
    src/setgid.c
    src/cross_root_unprivileged.c
    src/boundary.c
    src/matrix.c
    src/procfs_visibility.c
    src/nptl_sync.c
    src/core_dump_inhibit.c
    src/uid32_no_trunc.c
    src/errno_preservation.c)
# pthread for nptl_sync.c
target_link_libraries(test-uid-gid-direct-setters PRIVATE pthread)

target_include_directories(test-uid-gid-direct-setters PRIVATE src)
target_compile_options(test-uid-gid-direct-setters PRIVATE -Wall -Wextra -Werror)
# Build this stricter probe, but keep it out of the grouped CI pass condition
# until uid/gid setter boundary semantics match Linux.
install(TARGETS test-uid-gid-direct-setters RUNTIME DESTINATION usr/bin/starry-known-fail)
