set pagination off
set confirm off
set debuginfod enabled off
break native_marker
run
bt
continue
echo GDB_NATIVE_SMOKE_DONE\n
quit
