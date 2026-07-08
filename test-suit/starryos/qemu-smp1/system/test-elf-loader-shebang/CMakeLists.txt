cmake_minimum_required(VERSION 3.20)
project(test-elf-loader-shebang C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-elf-loader-shebang src/main.c)
target_include_directories(test-elf-loader-shebang PRIVATE src)
target_compile_options(test-elf-loader-shebang PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-elf-loader-shebang RUNTIME DESTINATION usr/bin/starry-test-suit)
