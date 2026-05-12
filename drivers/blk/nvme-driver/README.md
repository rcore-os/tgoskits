# NVME Driver

nvme driver 1.4

## example run

install qemu.

```shell
cargo install ostool
./img.sh

# run test with qemu
cargo test --test tests --  --show-output
```

## hardware test

1. 主机连接开发板串口
2. 开发板插入网线，并且主机与开发板应处于同一网段
3. 准备开发板设备树文件 `*.dtb`

```shell
cargo install ostool
cargo test --test tests --  --show-output --uboot
```
