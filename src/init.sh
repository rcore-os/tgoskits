echo @@@@@@@@@@ setup @@@@@@@@@@

./busybox mkdir -v /bin
./busybox ln -v -s /musl/busybox /bin/busybox
cd /bin
export PATH=/bin
busybox ln -v -s busybox ln
ln -v -s busybox cp
ln -v -s busybox mv
ln -v -s busybox rm
ln -v -s busybox cat
ln -v -s busybox touch
ln -v -s busybox sh
ln -v -s busybox ls
ln -v -s busybox env
ln -v -s busybox mkdir
ln -v -s busybox clear

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

# For loongarch glibc iozone
mkdir /usr
ln -v -s /lib /usr/lib64

echo @@@@@@@@@@ files @@@@@@@@@@
ls -lhAR /lib
echo @@@@@@@@@@ env @@@@@@@@@@
env
echo

echo @@@@@@@@@@ musl @@@@@@@@@@
cd /musl
./basic_testcode.sh
./lua_testcode.sh
./libctest_testcode.sh
./busybox_testcode.sh
./iozone_testcode.sh
# ./libcbench_testcode.sh

echo @@@@@@@@@@ glibc @@@@@@@@@@
cd /glibc
./basic_testcode.sh
./lua_testcode.sh
./libctest_testcode.sh
./busybox_testcode.sh
./iozone_testcode.sh
# ./libcbench_testcode.sh
