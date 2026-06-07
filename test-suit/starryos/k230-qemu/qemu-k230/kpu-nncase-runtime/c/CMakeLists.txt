cmake_minimum_required(VERSION 3.20)
project(k230-nncase-runtime-test C)

set(K230_KPU_NNCASE_APP_C_DIR
    "${CMAKE_CURRENT_SOURCE_DIR}/../../../../../../apps/starry/k230-kpu-nncase/c"
    CACHE PATH "Path to the Starry K230 KPU NNCase app C sources"
)

if(NOT EXISTS "${K230_KPU_NNCASE_APP_C_DIR}/CMakeLists.txt")
    message(FATAL_ERROR "K230 KPU NNCase app CMakeLists.txt was not found under ${K230_KPU_NNCASE_APP_C_DIR}")
endif()

add_subdirectory("${K230_KPU_NNCASE_APP_C_DIR}" "${CMAKE_CURRENT_BINARY_DIR}/k230-kpu-nncase-app")
