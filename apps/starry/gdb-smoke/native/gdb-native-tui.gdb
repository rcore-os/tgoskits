set pagination off
set confirm off
set debuginfod enabled off
directory /workspace/apps/starry/gdb-smoke/native/src
break native_marker
run
layout src
echo GDB_NATIVE_TUI_READY\n
echo Use "layout asm" or "layout regs" to switch layouts.\n
