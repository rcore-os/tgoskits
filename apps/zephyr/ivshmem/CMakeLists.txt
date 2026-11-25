# SPDX-License-Identifier: Apache-2.0

if(CONFIG_TGOS_IVC)
  if(NOT CONFIG_ARM64)
    message(FATAL_ERROR "TGOS_IVC currently supports only AArch64 Zephyr targets")
  endif()

  get_filename_component(TGOSKITS_ROOT
    "${CMAKE_CURRENT_LIST_DIR}/../../.." ABSOLUTE)
  set(TGOS_IVC_RUST_MANIFEST
    "${CMAKE_CURRENT_LIST_DIR}/rust/Cargo.toml")
  set(TGOS_IVC_RUST_TARGET "aarch64-unknown-none-softfloat")
  set(TGOS_IVC_RUST_TARGET_DIR
    "${CMAKE_CURRENT_BINARY_DIR}/rust-target")
  set(TGOS_IVC_RUST_LIB
    "${TGOS_IVC_RUST_TARGET_DIR}/${TGOS_IVC_RUST_TARGET}/release/libtgos_ivc_zephyr.a")

  find_program(TGOS_IVC_CARGO NAMES cargo REQUIRED)

  # The root rust-toolchain.toml is the single toolchain source. The nested
  # Cargo.lock is generated once by the integration owner and then enforced by
  # --locked for every firmware build.
  file(GLOB_RECURSE TGOS_IVC_RUST_INPUTS CONFIGURE_DEPENDS
    "${CMAKE_CURRENT_LIST_DIR}/rust/src/*.rs"
    "${TGOSKITS_ROOT}/virtualization/axivc/src/*.rs")
  list(APPEND TGOS_IVC_RUST_INPUTS
    "${TGOS_IVC_RUST_MANIFEST}"
    "${CMAKE_CURRENT_LIST_DIR}/rust/Cargo.lock"
    "${TGOSKITS_ROOT}/rust-toolchain.toml"
    "${TGOSKITS_ROOT}/virtualization/axivc/Cargo.toml"
    "${TGOSKITS_ROOT}/Cargo.toml")

  add_custom_command(
    OUTPUT "${TGOS_IVC_RUST_LIB}"
    COMMAND "${TGOS_IVC_CARGO}" build
      --manifest-path "${TGOS_IVC_RUST_MANIFEST}"
      --target "${TGOS_IVC_RUST_TARGET}"
      --target-dir "${TGOS_IVC_RUST_TARGET_DIR}"
      --locked
      --release
    DEPENDS ${TGOS_IVC_RUST_INPUTS}
    WORKING_DIRECTORY "${CMAKE_CURRENT_LIST_DIR}/rust"
    COMMENT "Building TGOSKits ivshmem Message V1 Rust adapter"
    VERBATIM)

  add_custom_target(tgos_ivc_rust_build
    DEPENDS "${TGOS_IVC_RUST_LIB}")

  zephyr_library()
  zephyr_library_sources(src/tgos_ivc_zephyr.c)
  zephyr_library_include_directories(include)
  zephyr_include_directories(include)
  zephyr_library_add_dependencies(tgos_ivc_rust_build)

  add_library(tgos_ivc_rust STATIC IMPORTED GLOBAL)
  set_target_properties(tgos_ivc_rust PROPERTIES
    IMPORTED_LOCATION "${TGOS_IVC_RUST_LIB}")
  add_dependencies(tgos_ivc_rust tgos_ivc_rust_build)
  zephyr_library_link_libraries(tgos_ivc_rust)
endif()
