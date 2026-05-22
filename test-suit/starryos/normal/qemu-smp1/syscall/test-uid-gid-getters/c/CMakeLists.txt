cmake_minimum_required(VERSION 3.20)
project(test-uid-gid-getters C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

# 6 个 getter syscall 地毯式覆盖：
#   getuid / geteuid / getgid / getegid / getresuid / getresgid
# 每模块自计 __pass/__fail，main 汇总；每 case 头部 man 原文引用。
add_executable(test-uid-gid-getters
    src/main.c
    src/getuid.c
    src/geteuid.c
    src/getgid.c
    src/getegid.c
    src/getresuid.c
    src/getresgid.c
    src/cross_consistency.c
    src/boundary.c
    src/matrix.c
    src/procfs_visibility.c
    src/uid32_no_trunc.c)

target_include_directories(test-uid-gid-getters PRIVATE src)
target_compile_options(test-uid-gid-getters PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-uid-gid-getters RUNTIME DESTINATION usr/bin/starry-test-suit)
