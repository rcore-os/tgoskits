cmake_minimum_required(VERSION 3.20)

set(STARRY_SOUND_COMMON_DIR "${CMAKE_CURRENT_LIST_DIR}")

function(starry_virtio_sound_case target main_source)
    if(NOT DEFINED STARRY_STAGING_ROOT)
        message(FATAL_ERROR "STARRY_STAGING_ROOT must be provided by axbuild")
    endif()

    foreach(tool aplay arecord)
        if(NOT EXISTS "${STARRY_STAGING_ROOT}/usr/bin/${tool}")
            message(FATAL_ERROR
                "${tool} is missing from ${STARRY_STAGING_ROOT}/usr/bin; "
                "alsa-utils v1.2.13 prebuild did not install it")
        endif()
    endforeach()
    if(NOT EXISTS "${STARRY_STAGING_ROOT}/usr/share/alsa/alsa.conf")
        message(FATAL_ERROR
            "alsa.conf is missing from ${STARRY_STAGING_ROOT}/usr/share/alsa")
    endif()
    if(NOT EXISTS "${STARRY_STAGING_ROOT}/usr/include/sound/asound.h")
        message(FATAL_ERROR
            "sound/asound.h is missing from ${STARRY_STAGING_ROOT}/usr/include")
    endif()

    add_library(${target}_common STATIC
        "${STARRY_SOUND_COMMON_DIR}/src/sound_test.c"
        "${STARRY_SOUND_COMMON_DIR}/src/wav.c"
    )
    target_include_directories(${target}_common
        PUBLIC "${STARRY_SOUND_COMMON_DIR}/include")
    target_compile_options(${target}_common PRIVATE -Wall -Wextra -Werror)
    target_link_libraries(${target}_common PUBLIC m)
    set_target_properties(${target}_common PROPERTIES
        C_STANDARD 11
        C_STANDARD_REQUIRED ON
        C_EXTENSIONS OFF)

    add_executable(${target} "${main_source}")
    target_link_libraries(${target} PRIVATE ${target}_common)
    target_compile_options(${target} PRIVATE -Wall -Wextra -Werror)
    set_target_properties(${target} PROPERTIES
        C_STANDARD 11
        C_STANDARD_REQUIRED ON
        C_EXTENSIONS OFF)

    install(TARGETS ${target} RUNTIME DESTINATION usr/bin/starry-test-suit)
    install(PROGRAMS
        "${STARRY_STAGING_ROOT}/usr/bin/aplay"
        "${STARRY_STAGING_ROOT}/usr/bin/arecord"
        DESTINATION usr/bin)
    install(DIRECTORY "${STARRY_STAGING_ROOT}/usr/share/alsa/"
        DESTINATION usr/share/alsa
        USE_SOURCE_PERMISSIONS)
    install(PROGRAMS "${STARRY_SOUND_COMMON_DIR}/sh/check-tools.sh"
        DESTINATION usr/bin/starry-test-suit
        RENAME virtio-sound-tools-check)
endfunction()
