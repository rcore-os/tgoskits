set pagination off
set confirm off
set debuginfod enabled off
set schedule-multiple on
break thread_marker
run
info threads
echo GDB_NATIVE_THREADS_LIST_DONE\n
shell pid="$(pidof gdb-native-thread-target)" && echo GDB_NATIVE_THREADS_TASK_BEGIN && ls "/proc/$pid/task" && echo GDB_NATIVE_THREADS_TASK_DONE
bt
echo GDB_NATIVE_THREADS_BT_DONE\n
delete breakpoints
continue
echo GDB_NATIVE_THREADS_DONE\n
quit
