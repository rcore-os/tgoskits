set pagination off
set confirm off
set debuginfod enabled off
set architecture i386:x86-64
set sysroot /
set solib-search-path /lib:/usr/lib
set remotetimeout 10
set remote hostio-open-packet off
set remote hostio-pread-packet off
target remote :1234
echo HOST_GDB_REMOTE_CONNECTED\n
break compute_value
continue
bt
echo HOST_GDB_REMOTE_BT_DONE\n
detach
echo HOST_GDB_REMOTE_DETACH_DONE\n
quit
