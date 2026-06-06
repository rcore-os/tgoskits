set pagination off
set confirm off
set debuginfod enabled off
set architecture riscv:rv64
set sysroot /
set solib-search-path /lib:/usr/lib
set remotetimeout 10
set remote hostio-open-packet off
set remote hostio-pread-packet off
target remote :1234
echo HOST_GDB_REMOTE_MANUAL_CONNECTED\n
