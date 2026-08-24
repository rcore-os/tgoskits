cmake_minimum_required(VERSION 3.20)
project(uaccess-lock-regression C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(uaccess-lock-regression src/main.c)
target_compile_options(uaccess-lock-regression PRIVATE -Wall -Wextra -Werror)
target_link_libraries(uaccess-lock-regression PRIVATE pthread)

install(TARGETS uaccess-lock-regression RUNTIME DESTINATION usr/bin)
