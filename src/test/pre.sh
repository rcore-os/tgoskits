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
mkdir -v -p /var/log

mkdir -v /etc
echo "root:x:0:0:root:/root:/bin/sh" >>/etc/passwd
echo "nobody:x:65534:65534:nobody:/:/bin/sh" >>/etc/passwd
echo "nameserver 8.8.8.8" > /etc/resolv.conf

echo @@@@@@@@@@ files @@@@@@@@@@
ls -lhAR /lib
echo @@@@@@@@@@ env @@@@@@@@@@
env
echo

run_ltp() {
    echo "#### OS COMP TEST GROUP START ltp-$1 ####"

    export LTP_TIMEOUT_MUL=0.5
    export LTP_DEV_FS_TYPE=tmpfs
    export LTP_SINGLE_FS_TYPE=tmpfs

    all_testcases="
    accept01
    accept03
    access01
    alarm02
    alarm03
    alarm05
    alarm06
    alarm07
    bind01
    bind05
    chdir01
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
    exit_group01
    exit02
    faccessat01
    faccessat02
    faccessat201
    faccessat202
    fchdir01
    fchdir02
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
    fdatasync01
    fdatasync02
    fork01
    fork03
    fork07
    fork08
    fork10
    fpathconf01
    fstat02
    fstat02_64
    fstat03
    fstat03_64
    fstatfs01
    fstatfs01_64
    fstatfs02
    fstatfs02_64
    fsync01
    ftruncate01
    ftruncate01_64
    futex_cmp_requeue02
    futex_wait01
    futex_wait02
    futex_wait04
    futex_wake01
    futex_wake02
    futex_wake03
    futex_wake04
    getcwd01
    getcwd03
    getdents01
    getdents02
    getdomainname01
    getegid01
    getegid01_16
    geteuid01
    geteuid02
    gethostname01
    getitimer01
    getitimer02
    getpagesize01
    getpeername01
    getpgid01
    getpgid02
    getpgrp01
    getpid01
    getpid02
    getppid01
    getppid02
    getpriority01
    getpriority02
    getrandom01
    getrandom02
    getrandom03
    getrandom04
    getrlimit01
    getrlimit02
    getrusage01
    getrusage02
    getsid01
    getsid02
    gettid01
    gettid02
    gettimeofday01
    getuid01
    getuid03
    ioctl04
    ioctl05
    ioctl06
    kill03
    kill06
    kill07
    kill08
    kill09
    link02
    link04
    link05
    link08
    llseek01
    llseek02
    llseek03
    lseek01
    lseek02
    lseek07
    lseek11
    lstat01
    lstat01_64
    lstat02
    lstat02_64
    madvise01
    madvise02
    madvise03
    madvise05
    madvise08
    madvise10
    madvise11
    memcmp01
    memcpy01
    memset01
    mkdir05
    mkdirat02
    mmap02
    mmap05
    mmap06
    mmap09
    mmap17
    mmap19
    nanosleep04
    open01
    open02
    open03
    open04
    open06
    open07
    open08
    open09
    open10
    open11
    openat01
    pathconf01
    pathconf02
    pipe01
    pipe02
    pipe03
    pipe06
    pipe07
    pipe08
    pipe10
    pipe11
    pipe12
    pipe14
    pipe15
    pipe2_01
    pipe2_04
    poll01
    ppoll01
    pread01
    pread01_64
    pread02
    pread02_64
    preadv01
    preadv01_64
    pselect02
    pselect02_64
    pselect03
    pselect03_64
    pwrite01
    pwrite01_64
    pwrite02
    pwrite02_64
    pwrite03
    pwrite03_64
    pwrite04
    pwrite04_64
    pwritev01
    pwritev01_64
    read01
    read02
    read03
    read04
    readdir01
    readlink01
    readlinkat01
    readlinkat02
    readv01
    readv02
    rmdir01
    rmdir02
    rmdir03
    rt_sigaction03
    rt_sigprocmask01
    rt_sigprocmask02
    sbrk01
    sbrk02
    select03
    sendfile02
    sendfile02_64
    sendfile04
    sendfile04_64
    sendfile05
    sendfile05_64
    sendfile06
    sendfile06_64
    sendfile08
    sendfile08_64
    setitimer01
    setitimer02
    setpgrp02
    setrlimit02
    setrlimit03
    setrlimit04
    setrlimit05
    setsockopt03
    setsockopt04
    setuid01
    shmat01
    shmat02
    shmat03
    shmat04
    shmctl01
    shmctl03
    shmctl07
    shmctl08
    shmdt01
    shmdt02
    shmem_2nstest
    shmget02
    shmget03
    shmget04
    shmget05
    shmget06
    shmnstest
    sigaltstack02
    signal01
    signal02
    signal03
    signal04
    signal05
    sigpending02
    socket01
    socket02
    splice01
    splice02
    splice03
    splice05
    splice06
    splice07
    splice08
    splice09
    stat01
    stat01_64
    stat02
    stat02_64
    stat03
    stat03_64
    statfs02
    statfs02_64
    statvfs02
    statx01
    statx02
    statx03
    symlink02
    symlink04
    syscall01
    tgkill03
    tkill01
    tkill02
    truncate02
    truncate02_64
    uname01
    uname02
    uname04
    unlink05
    unlink07
    unlink08
    unlink09
    unlinkat01
    utime06
    utime07
    utimes01
    utsname01
    utsname04
    wait01
    wait02
    wait401
    wait402
    wait403
    waitpid01
    waitpid03
    waitpid04
    waitpid06
    waitpid07
    waitpid08
    waitpid10
    waitpid11
    waitpid12
    waitpid13
    write01
    write02
    write03
    write04
    write05
    write06
    writev07
    "

    cd ltp/testcases/bin
    for f in $all_testcases; do
        echo "RUN LTP CASE $f"
        ./$f
        echo "FAIL LTP CASE $f : 0"
    done
    cd ../../..

    echo "#### OS COMP TEST GROUP END ltp-$1 ####"
}

cd /musl
run_ltp musl

cd /glibc
run_ltp glibc

cd /musl
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

cd /musl
timeout 60 ./iperf_testcode.sh
cd /glibc
timeout 60 ./iperf_testcode.sh

cd /musl
timeout 60 ./netperf_testcode.sh
cd /glibc
timeout 60 ./netperf_testcode.sh

cd /tmp
ln -v -s /musl/busybox busybox
ln -v -s /musl/iozone iozone
timeout 300 /musl/iozone_testcode.sh
ln -v -s -f /glibc/iozone iozone
timeout 300 /glibc/iozone_testcode.sh

cd /musl
timeout 300 ./lmbench_testcode.sh
cd /glibc
timeout 300 ./lmbench_testcode.sh
