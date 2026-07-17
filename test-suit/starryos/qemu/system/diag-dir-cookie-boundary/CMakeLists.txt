cmake_minimum_required(VERSION 3.20)
project(diag-dir-cookie-boundary C)

add_executable(diag-dir-cookie-boundary src/main.c)
target_compile_options(diag-dir-cookie-boundary PRIVATE -Wall -Wextra -Werror)
install(TARGETS diag-dir-cookie-boundary RUNTIME DESTINATION usr/bin/starry-test-suit)
