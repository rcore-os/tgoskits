cmake_minimum_required(VERSION 3.21)
project(orangepi_iperf2_assets NONE)
set(BOARD_SMOKE_SCRIPT "${CMAKE_CURRENT_LIST_DIR}/../board-smoke.sh")
include("${CMAKE_CURRENT_LIST_DIR}/../../../board-common/iperf2/CMakeLists.txt")
