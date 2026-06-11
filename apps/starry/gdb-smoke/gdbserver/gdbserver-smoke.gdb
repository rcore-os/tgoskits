set pagination off
set confirm off
set debuginfod enabled off
set sysroot /
set solib-search-path /lib:/usr/lib
set remotetimeout 10
set remote hostio-open-packet off
set remote hostio-pread-packet off
target remote 127.0.0.1:1234
break compute_value
continue
bt
echo GDBSERVER_BREAKPOINT_DONE\n
delete breakpoints
continue
echo GDBSERVER_CONTINUE_DONE\n
quit
