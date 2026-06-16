cmake_minimum_required(VERSION 3.20)
project(test-uid-gid-re-setters C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

# Group C: setreuid / setregid 两参数 setter (real + effective)
add_executable(test-uid-gid-re-setters
    src/main.c
    src/setreuid.c
    src/setregid.c
    src/saved_id_semantics.c
    src/boundary.c
    src/matrix.c
    src/procfs_visibility.c
    src/nptl_sync.c
    src/core_dump_inhibit.c
    src/errno_preservation.c)
target_link_libraries(test-uid-gid-re-setters PRIVATE pthread)

target_include_directories(test-uid-gid-re-setters PRIVATE src)
target_compile_options(test-uid-gid-re-setters PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-uid-gid-re-setters RUNTIME DESTINATION usr/bin/starry-test-suit)
