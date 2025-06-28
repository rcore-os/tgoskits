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

echo @@@@@@@@@@ musl @@@@@@@@@@
cd /musl
./lmbench_testcode.sh
./iozone_testcode.sh
./libcbench_testcode.sh
./libctest_testcode.sh
./busybox_testcode.sh
./basic_testcode.sh
./lua_testcode.sh

echo @@@@@@@@@@ glibc @@@@@@@@@@
cd /glibc
./lmbench_testcode.sh
./iozone_testcode.sh
./libcbench_testcode.sh
./busybox_testcode.sh
./basic_testcode.sh
./lua_testcode.sh
