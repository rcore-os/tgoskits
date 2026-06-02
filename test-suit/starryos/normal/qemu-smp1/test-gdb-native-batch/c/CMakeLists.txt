cmake_minimum_required(VERSION 3.20)
project(test-gdb-native-batch C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-gdb-native-batch-target src/main.c)
target_compile_options(test-gdb-native-batch-target PRIVATE -Wall -Wextra -Werror -O0 -g)

set(STAGING_USR_BIN "${STARRY_STAGING_ROOT}/usr/bin")
set(STAGING_USR_LIB "${STARRY_STAGING_ROOT}/usr/lib")
set(STAGING_USR_SHARE "${STARRY_STAGING_ROOT}/usr/share")

install(TARGETS test-gdb-native-batch-target RUNTIME DESTINATION usr/bin)
install(FILES gdb-native-batch.gdb DESTINATION usr/bin)
install(PROGRAMS run-gdb-native-batch.sh DESTINATION usr/bin)

foreach(bin IN ITEMS gdb python python3 python3.12)
    if(EXISTS "${STAGING_USR_BIN}/${bin}")
        install(PROGRAMS "${STAGING_USR_BIN}/${bin}" DESTINATION usr/bin)
    endif()
endforeach()

if(EXISTS "${STAGING_USR_LIB}/python3.12")
    install(DIRECTORY "${STAGING_USR_LIB}/python3.12" DESTINATION usr/lib)
endif()

if(EXISTS "${STAGING_USR_LIB}/python312.zip")
    install(FILES "${STAGING_USR_LIB}/python312.zip" DESTINATION usr/lib)
endif()

file(GLOB LIBPYTHON_FILES "${STAGING_USR_LIB}/libpython3.12.so*")
if(LIBPYTHON_FILES)
    install(FILES ${LIBPYTHON_FILES} DESTINATION usr/lib)
endif()

if(EXISTS "${STAGING_USR_SHARE}/gdb")
    install(DIRECTORY "${STAGING_USR_SHARE}/gdb" DESTINATION usr/share)
endif()
