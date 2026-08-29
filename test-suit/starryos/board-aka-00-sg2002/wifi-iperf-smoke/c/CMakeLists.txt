cmake_minimum_required(VERSION 3.20)
project(wifi_session_helper C)

add_executable(wifi-session-helper ../../../common/wifi-session-helper.c)
target_link_options(wifi-session-helper PRIVATE -static)
install(TARGETS wifi-session-helper RUNTIME DESTINATION bin)
