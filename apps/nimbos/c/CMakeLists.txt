cmake_minimum_required(VERSION 3.5)

project(nimbos_user)
enable_language(C ASM)
set(CMAKE_OSX_DEPLOYMENT_TARGET "")

# Toolchain
set(PREFIX ${ARCH}-linux-musl-)

if (${ARCH} STREQUAL x86_64)
    set(CMAKE_C_FLAGS       "-mno-sse")
elseif (${ARCH} STREQUAL riscv32)
    set(CMAKE_C_FLAGS       "-march=rv32imac -mabi=ilp32 -mcmodel=medany")
elseif (${ARCH} STREQUAL riscv64)
    set(CMAKE_C_FLAGS       "-march=rv64imac -mabi=lp64 -mcmodel=medany")
elseif (${ARCH} STREQUAL aarch64)
    set(CMAKE_C_FLAGS       "-mgeneral-regs-only")
elseif (${ARCH} STREQUAL loongarch64)
    set(CMAKE_C_FLAGS       "-march=loongarch64")
else()
    message("Unsupported arch: ${ARCH}")
endif ()
set(CMAKE_ASM_COMPILER      ${PREFIX}gcc)
set(CMAKE_C_COMPILER        ${PREFIX}gcc)
set(CMAKE_OBJDUMP           ${PREFIX}objdump)
set(CMAKE_RANLIB            ${PREFIX}ranlib)
set(CMAKE_C_FLAGS           "${CMAKE_C_FLAGS} -static -no-pie -fno-builtin -nostdinc -fno-stack-protector -ggdb -Wall")
set(CMAKE_ASM_FLAGS         ${CMAKE_C_FLAGS})
set(CMAKE_C_LINK_FLAGS      "${LINK_FLAGS} -nostdlib")  # override default value to get rid of '-Wl,-search_paths_first -Wl,-headerpad_max_install_names'
set(CMAKE_SHARED_LIBRARY_LINK_C_FLAGS)                  # override default value to get rid of '-rdynamic' on Linux

# Path
if (${ARCH} STREQUAL riscv32 OR ${ARCH} STREQUAL riscv64)
    set(ARCH_DIR lib/arch/riscv)
else()
    set(ARCH_DIR lib/arch/${ARCH})
endif()

set(ASM_DIR ${ARCH}/asm)
set(BIN_DIR ${ARCH}/bin)

# Library
aux_source_directory(lib LIBS)
set(LIBS ${ARCH_DIR}/crt.S ${ARCH_DIR}/clone.S ${LIBS})
add_library(ulib ${LIBS} syscall_ids)
include_directories(include/)
target_include_directories(ulib PRIVATE ${ARCH_DIR})

# Execuatble
aux_source_directory(src SRCS)
set(EXECUTABLE_OUTPUT_PATH ${BIN_DIR})
foreach(PATH ${SRCS})
    get_filename_component(NAME ${PATH} NAME_WE)
    add_executable(${NAME} ${PATH})
    target_link_libraries(${NAME} ulib)

    add_custom_command(
        TARGET ${NAME}
        POST_BUILD
        COMMAND mkdir -p ${ASM_DIR}
        COMMAND ${CMAKE_OBJDUMP} ARGS -d $<TARGET_FILE:${NAME}> > ${ASM_DIR}/${NAME}.asm
    )
endforeach()

# If arch is not x86_64, we need to use different syscall ids
if (NOT ${ARCH} STREQUAL x86_64)
    add_custom_command(
        OUTPUT syscall_ids.h
        COMMAND sed ARGS -n -e s/__NR_/SYS_/p
            < ${CMAKE_SOURCE_DIR}/lib/syscall_ids.h.no_x86.in
            > ${CMAKE_SOURCE_DIR}/lib/syscall_ids.h
    )
else()
    add_custom_command(
        OUTPUT syscall_ids.h
        COMMAND sed ARGS -n -e s/__NR_/SYS_/p
            < ${CMAKE_SOURCE_DIR}/lib/syscall_ids.h.in
            > ${CMAKE_SOURCE_DIR}/lib/syscall_ids.h
    )
endif()