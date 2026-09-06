#!/bin/sh
# net-bench SG2002 board init script.
#
# The board boots with WiFi AP enabled via the aic8800 feature.  Actual network
# testing is driven externally from a PC via paramiko SSH (see board/board-controller.py).
# This init script serves as a boot-complete sentinel and nothing more.
echo STARRY_SG2002_BOOT_OK
