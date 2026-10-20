cmake_minimum_required(VERSION 3.15)
project(axvisor_yolo_images LANGUAGES C CXX)
set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 11)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
# All source, model, headers and vendor libraries belong to this package.
set(DEMO "${CMAKE_CURRENT_SOURCE_DIR}")
set(DEPS "${DEMO}/3rdparty")
set(INCLUDES "${DEMO}/src" "${DEMO}/utils" "${DEPS}/rknpu2/include")

if(NOT CMAKE_SYSTEM_PROCESSOR MATCHES "^(aarch64|arm64)$")
  message(FATAL_ERROR "Runtime requires AArch64 Linux. Use build.sh.")
endif()
find_package(Threads REQUIRED)
add_executable(rknn_yolov8_images src/main.cc
  "${DEMO}/src/rknpu2/yolov8.cc" "${DEMO}/src/postprocess.cc"
  "${DEMO}/utils/image_utils.c" "${DEMO}/utils/file_utils.c")
target_include_directories(rknn_yolov8_images PRIVATE ${INCLUDES}
  "${DEPS}/stb_image" "${DEPS}/jpeg_turbo/include" "${DEPS}/librga/include")
target_compile_definitions(rknn_yolov8_images PRIVATE LIBRGA_IM2D_HANDLE)
target_link_libraries(rknn_yolov8_images PRIVATE
  "${DEPS}/rknpu2/Linux/aarch64/librknnrt.so"
  "${DEPS}/librga/Linux/aarch64/librga.so"
  "${DEPS}/jpeg_turbo/Linux/aarch64/libturbojpeg.a" Threads::Threads dl m)
set_target_properties(rknn_yolov8_images PROPERTIES INSTALL_RPATH "$ORIGIN/lib")
install(TARGETS rknn_yolov8_images DESTINATION .)
install(FILES "${DEPS}/rknpu2/Linux/aarch64/librknnrt.so"
  "${DEPS}/librga/Linux/aarch64/librga.so" DESTINATION lib)
install(DIRECTORY "${DEMO}/model/" DESTINATION model)
install(DIRECTORY images/ DESTINATION images)
install(PROGRAMS yolov8_demo.sh DESTINATION .)
install(FILES README.md DESTINATION .)
install(FILES LICENSE THIRD_PARTY_NOTICES.md DESTINATION licenses)
install(FILES "${DEPS}/jpeg_turbo/LICENSE.md" DESTINATION licenses RENAME libjpeg-turbo.md)
install(FILES "${DEPS}/stb_image/LICENSE.txt" DESTINATION licenses RENAME stb.txt)
