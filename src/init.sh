echo @@@@@@@@@@ setup @@@@@@@@@@

/musl/busybox mkdir -v /bin
/musl/busybox --install -s /bin
export PATH=/bin

mkdir -v /lib
ln -v -s /glibc/lib/libc.so.6 /lib/libc.so.6
ln -v -s /glibc/lib/libm.so.6 /lib/libm.so.6
ln -v -s /lib/libc.so.6 /lib/libc.so
ln -v -s /lib/libm.so.6 /lib/libm.so
if [[ $ARCH == loongarch64 ]]; then
    ln -v -s /musl/lib/libc.so /lib/ld-musl-loongarch-lp64d.so.1
    ln -v -s /glibc/lib/ld-linux-loongarch-lp64d.so.1 /lib/ld-linux-loongarch-lp64d.so.1
elif [[ $ARCH == riscv64 ]]; then
    ln -v -s /musl/lib/libc.so /lib/ld-musl-riscv64.so.1
    ln -v -s /musl/lib/libc.so /lib/ld-musl-riscv64-sf.so.1
    ln -v -s /glibc/lib/ld-linux-riscv64-lp64d.so.1 /lib/ld-linux-riscv64-lp64d.so.1
fi
ln -v -s /lib /lib64

mkdir -v /usr
ln -v -s /lib /usr/lib64

mkdir -v -p /var/tmp

echo @@@@@@@@@@ files @@@@@@@@@@
ls -lhAR /lib
echo @@@@@@@@@@ env @@@@@@@@@@
env
echo

run_ltp() {
    echo "#### OS COMP TEST GROUP START ltp-$1 ####"

    all_testcases="
    accept01
    accept03
    alarm02
    alarm03
    alarm05
    alarm06
    alarm07
    bind01
    bind05
    chdir04
    chmod01
    chown01
    clock_getres01
    clock_gettime02
    clock_nanosleep01
    clock_nanosleep04
    clone01
    clone03
    clone06
    close01
    close02
    confstr01
    creat01
    creat05
    dirtypipe
    dup01
    dup02
    dup03
    dup04
    dup07
    dup201
    dup202
    dup203
    dup204
    dup205
    dup206
    dup207
    dup3_01
    dup3_02
    execve03
    exit02
    exit_group01
    faccessat01
    faccessat02
    faccessat201
    faccessat202
    fchmod01
    fchmod02
    fchmod03
    fchmod04
    fchmod05
    fchmod06
    fchmodat01
    fchmodat02
    fchown01
    fchown02
    fchown03
    fchown04
    fchown05
    fcntl02
    fcntl02_64
    fcntl03
    fcntl03_64
    fcntl04
    fcntl04_64
    fcntl05
    fcntl05_64
    fcntl08
    fcntl08_64
    fcntl27
    fcntl27_64
    fcntl29
    fcntl29_64
    fork01
    fork03
    fork07
    fork08
    fork10
    getpid01
    waitpid01
    futex_cmp_requeue02
    futex_wait01
    futex_wait02
    futex_wait03
    futex_wait04
    futex_wake01
    futex_wake02
    futex_wake03
    futex_wake04
    "
    all_testcases="
    chdir01
    "

    for f in $all_testcases; do
        echo "RUN LTP CASE $f"
        ltp/testcases/bin/$f
        ret=$?
        echo "FAIL LTP CASE $f : $ret"
    done

    echo "#### OS COMP TEST GROUP END ltp-$1 ####"
}

export LTP_DEV_FS_TYPE=tmpfs
export LTP_SINGLE_FS_TYPE=tmpfs

cd /musl
run_ltp musl
exit

timeout 20 ./basic_testcode.sh
timeout 20 ./lua_testcode.sh
timeout 30 ./busybox_testcode.sh

cd /glibc
timeout 20 ./basic_testcode.sh
timeout 20 ./lua_testcode.sh
timeout 30 ./busybox_testcode.sh

cd /musl
timeout 60 ./libctest_testcode.sh
timeout 60 ./libcbench_testcode.sh

cd /glibc
timeout 60 ./libcbench_testcode.sh

ln -v -s /musl/busybox /tmp/busybox
cd /tmp
cp /musl/iozone_testcode.sh /musl/iozone .
timeout 300 ./iozone_testcode.sh
cp /glibc/iozone_testcode.sh /glibc/iozone .
timeout 300 ./iozone_testcode.sh

cd /musl
timeout 300 ./lmbench_testcode.sh

cd /glibc
timeout 300 ./lmbench_testcode.sh
