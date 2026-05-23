cmake_minimum_required(VERSION 3.20)
project(test-path-family C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
add_executable(
    test-path-family
    src/main.c
    src/mkdirat.c
    src/getcwd.c
    src/chdir.c
    src/unlinkat.c
    src/rename.c
    src/renameat2.c
    src/linkat.c
    src/symlink_readlink.c
    src/getdents64.c
    src/fchdir.c
    src/perm_matrix.c
    src/mknodat.c
    src/chroot.c
)
target_include_directories(test-path-family PRIVATE src)
target_compile_options(test-path-family PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-path-family RUNTIME DESTINATION usr/bin/starry-test-suit)
