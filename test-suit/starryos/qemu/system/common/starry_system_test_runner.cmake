add_executable(
    starry-run-system-tests
    "${CMAKE_CURRENT_LIST_DIR}/starry_system_test_runner.c")
target_compile_options(starry-run-system-tests PRIVATE -Wall -Wextra -Werror)
install(TARGETS starry-run-system-tests RUNTIME DESTINATION usr/bin)
