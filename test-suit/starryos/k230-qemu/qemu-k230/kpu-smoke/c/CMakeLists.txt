cmake_minimum_required(VERSION 3.20)
project(k230-kpu-smoke C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
add_executable(k230-kpu-smoke src/main.c)
target_include_directories(k230-kpu-smoke PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}/../../../../../../drivers/npu/k230-kpu/include"
)
target_compile_options(k230-kpu-smoke PRIVATE -Wall -Wextra -Werror)
install(TARGETS k230-kpu-smoke RUNTIME DESTINATION usr/bin)
install(FILES
    assets/runtime-direct-io.krun
    assets/runtime-direct-io-file.krun
    assets/runtime-direct-source.bin
    DESTINATION usr/share/k230-kpu-smoke
)

set(K230_KPU_REAL_KMODEL "${CMAKE_CURRENT_SOURCE_DIR}/assets/kmodels/yolov8n_320.kmodel")
set(K230_KPU_REAL_KMODEL_SHA256 "${K230_KPU_REAL_KMODEL}.sha256")
if(EXISTS "${K230_KPU_REAL_KMODEL}")
    install(FILES "${K230_KPU_REAL_KMODEL}" DESTINATION usr/share/k230-kpu-smoke/models)
    if(EXISTS "${K230_KPU_REAL_KMODEL_SHA256}")
        install(FILES "${K230_KPU_REAL_KMODEL_SHA256}" DESTINATION usr/share/k230-kpu-smoke/models)
    endif()
endif()

set(K230_KPU_CAPTURE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/assets/captures")
if(EXISTS "${K230_KPU_CAPTURE_DIR}/yolov8n-full-sequence-delta.krun")
    install(DIRECTORY "${K230_KPU_CAPTURE_DIR}/" DESTINATION usr/share/k230-kpu-smoke/captures)
endif()
