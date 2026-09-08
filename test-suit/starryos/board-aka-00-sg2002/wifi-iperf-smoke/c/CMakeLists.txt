cmake_minimum_required(VERSION 3.21)
project(aka_iperf2_assets NONE)
set(BOARD_SMOKE_SCRIPT "${CMAKE_CURRENT_LIST_DIR}/../iperf-smoke.sh")
include("${CMAKE_CURRENT_LIST_DIR}/../../../board-common/iperf2/CMakeLists.txt")
