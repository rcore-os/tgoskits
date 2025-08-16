if ! /musl/busybox test -d /bin; then
    echo @@@@@@@@@@ setup @@@@@@@@@@

    /musl/busybox mkdir -v /bin
    /musl/busybox --install -s /bin
    export PATH=/bin

    mkdir -p /root
fi

echo @@@@@@@@@@ env @@@@@@@@@@
env
echo

cd /musl
./git_testcode.sh

cd /glibc
./git_testcode.sh
