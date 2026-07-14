cmake_minimum_required(VERSION 3.20)
project(test-x86-avx-ctxsw C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-x86-avx-ctxsw src/main.c)
target_include_directories(test-x86-avx-ctxsw PRIVATE src)
target_compile_options(test-x86-avx-ctxsw PRIVATE -Wall -Wextra -pthread)
target_link_options(test-x86-avx-ctxsw PRIVATE -pthread)

# The inline asm names ymm registers; allow AVX codegen on x86_64 so the YMM
# clobbers are accepted. Other architectures compile the skip path.
if(CMAKE_C_COMPILER_TARGET MATCHES "x86_64")
    target_compile_options(test-x86-avx-ctxsw PRIVATE -mavx)
endif()

install(TARGETS test-x86-avx-ctxsw RUNTIME DESTINATION usr/bin/starry-test-suit)
