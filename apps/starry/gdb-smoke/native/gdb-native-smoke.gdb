set pagination off
set confirm off
set debuginfod enabled off
break native_marker
run
bt
echo GDB_NATIVE_BACKTRACE_DONE\n
info proc mappings
echo GDB_NATIVE_PROC_MAPPINGS_DONE\n
info files
echo GDB_NATIVE_PROC_FILES_DONE\n
info auxv
echo GDB_NATIVE_PROC_AUXV_DONE\n
shell pid="$(pidof gdb-native-smoke-target)" && echo GDB_NATIVE_PROC_STATUS_BEGIN && cat "/proc/$pid/status" && echo GDB_NATIVE_PROC_STATUS_DONE
info registers
echo GDB_NATIVE_REGS_DONE\n
x/4gx $sp
echo GDB_NATIVE_MEMORY_DONE\n
stepi
echo GDB_NATIVE_STEPI_DONE\n
continue
echo GDB_NATIVE_SMOKE_DONE\n
quit
