cmake_minimum_required(VERSION 3.21)

# Board asset pipeline for the UVC + RKNN benchmark.
#
# `starry app board` uploads every regular file this project installs as a board
# session file, and the board downloads it from the board session HTTP endpoint.
# The image runner is a tree of a dozen binaries, libraries, model and
# validation files, so the case publishes it as one deploy archive that
# `init.sh` fetches and unpacks instead of asking the board for every file
# separately.
#
# The download is optional: the board network can still be unavailable while
# `init.sh` retries it, so a failed download falls back to the install tree that
# is predeployed in the persistent Linux rootfs at /guest/npu_demo/ci-v2.
#
# The archive contents come from `build-image-runner.sh`, which cross compiles
# the aarch64 runner together with its vendored RKNN/RGA libraries and installs
# them under `rknn-yolov8-image/install/`. This project only verifies that
# install tree and packs it, so no binary is stored in the repository and the
# deployed runner always matches the current checkout.
project(rknn_yolov8_image_deploy NONE)

set(image_install_dir
    "${CMAKE_CURRENT_SOURCE_DIR}/../rknn-yolov8-image/install/rk3588_linux_aarch64/rknn_yolov8_image")

if(NOT IS_DIRECTORY "${image_install_dir}")
    message(FATAL_ERROR
        "the UVC + RKNN image runner install tree is missing: ${image_install_dir}; "
        "run benchmarks/starry/orangepi-5-plus-uvc-rknn/build-image-runner.sh "
        "before this pipeline")
endif()

# The fixed-image validation assets belong to the benchmark verdict, so an
# install tree without them must fail here instead of on the board.
foreach(required_path IN ITEMS
        rknn_yolov8_image
        rknn_yolov8_stream
        rknn_yolov8_bench
        model/yolov8.rknn
        model/coco_80_labels_list.txt
        validation/images.txt
        validation/expected.txt)
    if(NOT EXISTS "${image_install_dir}/${required_path}")
        message(FATAL_ERROR
            "the UVC + RKNN image runner install tree is incomplete: "
            "${image_install_dir}/${required_path} is missing")
    endif()
endforeach()

file(GLOB validation_images "${image_install_dir}/validation/*.jpg")
if(NOT validation_images)
    message(FATAL_ERROR
        "the UVC + RKNN image runner install tree has no validation images: "
        "${image_install_dir}/validation/*.jpg")
endif()

# `lib/` holds the vendored RKNN, RGA and UVC runtime the deployed binaries
# resolve next to themselves; an empty library directory would only fail on the
# board.
file(GLOB image_libraries "${image_install_dir}/lib/*.so*")
if(NOT image_libraries)
    message(FATAL_ERROR
        "the UVC + RKNN image runner install tree has no runtime libraries: "
        "${image_install_dir}/lib/*.so*")
endif()

find_program(TAR_EXECUTABLE tar REQUIRED)
find_program(GZIP_EXECUTABLE gzip REQUIRED)

# The archive is packed from the install tree with fixed ownership, modes and
# timestamps so the same checkout produces the same bytes, and `gzip -n` keeps
# the timestamp out of the compressed stream. The install tree is produced
# outside CMake, so this target always packs the current tree instead of
# depending on a cached output.
set(deploy_archive "${CMAKE_CURRENT_BINARY_DIR}/rknn-yolov8-deploy.tar.gz")
add_custom_target(rknn_yolov8_image_deploy_archive ALL
    COMMAND "${TAR_EXECUTABLE}" --sort=name --mtime=@0 --owner=0 --group=0
            --numeric-owner --mode=u+rwX,go+rX,go-w
            -cf "${CMAKE_CURRENT_BINARY_DIR}/rknn-yolov8-deploy.tar" -- .
    COMMAND "${GZIP_EXECUTABLE}" -n -f
            "${CMAKE_CURRENT_BINARY_DIR}/rknn-yolov8-deploy.tar"
    WORKING_DIRECTORY "${image_install_dir}"
    COMMENT "packing the UVC + RKNN image runner deploy archive"
    VERBATIM)

# The destination must stay a non-empty relative directory: the board runner
# installs with `DESTDIR=<board session upload root>`, and `DESTINATION .`
# normalizes to an empty target path that fails install with
# "file INSTALL given no DESTINATION". The board config offers this archive as
# the same relative path.
install(FILES "${deploy_archive}" DESTINATION share/uvc-rknn)
