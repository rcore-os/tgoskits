set pagination off
set confirm off
set debuginfod enabled off
break native_marker
run
bt
info registers
continue
quit
