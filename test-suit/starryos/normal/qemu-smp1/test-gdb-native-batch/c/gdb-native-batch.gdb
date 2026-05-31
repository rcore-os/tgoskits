set pagination off
set confirm off
set debuginfod enabled off
break native_marker
run
bt
info registers
continue
echo GDB_NATIVE_BATCH_DONE\n
quit
