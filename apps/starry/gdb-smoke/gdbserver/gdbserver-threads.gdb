set pagination off
set confirm off
set debuginfod enabled off
set sysroot /
set solib-search-path /lib:/usr/lib
set remotetimeout 10
set remote hostio-open-packet off
set remote hostio-pread-packet off
target remote 127.0.0.1:1234
echo GDBSERVER_THREADS_CONNECTED\n
break thread_marker
echo GDBSERVER_THREADS_BREAKPOINT_SET\n
info threads
echo GDBSERVER_THREADS_INITIAL_LIST_DONE\n
continue
echo GDBSERVER_THREADS_BREAKPOINT_HIT\n
info threads
echo GDBSERVER_THREADS_LIST_DONE\n
bt
echo GDBSERVER_THREADS_BT_DONE\n
delete breakpoints
echo GDBSERVER_THREADS_BREAKPOINTS_DELETED\n
continue
echo GDBSERVER_THREADS_PENDING_TRAP_CONSUMED\n
continue
echo GDBSERVER_THREADS_CONTINUE_DONE\n
quit
