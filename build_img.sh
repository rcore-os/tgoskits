#!/bin/sh
################################################################
# 	MacOS  : brew install dosfstools
# 	Ubuntu : apt-get install dosfstools
#	Usage:
# 		build_img.sh -a [arch] -fs [ext4|fat32] -file [testcase] -s [size]
################################################################
# default setting
arch=x86_64
fs=fat32
size=30
FILE=

display_help()
{
	echo ""
	echo "./build_img.sh -a [arch] -fs [filesystem] -file [testcase]"
	# 若不指定参数，则使用默认的测例
	echo "  -a | --arch		architecture: x86_64|riscv64|aarch64", default is x86_64
	echo "  -fs | --filesystem	filesystem: ext4|fat32", default is fat32
	echo "  -file | --testcase  If not specified, use the default testcases for different architectures."
	echo "  -s | --size		size of the disk image in 4MB batch size, default is set to 30, which means 120MB disk image"
	echo "  default testcases:"
	echo "    x86_64: x86_64_linux_musl"
	echo "    riscv64: riscv64_linux_musl"
	echo "    aarch64: aarch64-linux-musl"
	echo "  -h | --help		display help"
	echo ""
	exit 1
}

# 可能接受四类参数 -a [arch] -fs [filesystem] -file [testcase] -s [size]
# 但是不一定只有一个参数，所以使用 while 循环
while [ "$1" != "" ]; do
	case $1 in
		-a | --arch )	shift
						arch=$1
						;;
		-fs | --filesystem )	shift
						fs=$1
						;;
		-file | --testcase )	shift
						FILE=$1
						;;
		-s | --size )		shift
						size=$1
						;;
		-h | --help )		display_help
						exit
						;;
		* )					display_help
						exit 1
	esac
	shift
done

if [ -z "$FILE" ]; then # use default testcases
	if [ "$arch" = "riscv64" ]; then
		FILE=riscv64_linux_musl
	elif [ "$arch" = "x86_64" ]; then
		FILE=x86_64_linux_musl
	elif [ "$arch" = "aarch64" ]; then
		FILE=aarch64-linux-musl
	elif [ "$arch" = "aarch64_be" ]; then
		FILE=loongarch64-linux-musl
	else
		echo "Unknown architecture: $arch"
		exit 1
	fi
fi

# 如果 testcases 下对应测例不存在，执行 submodules 拉取
if [ ! -d "./testcases/$FILE" ]; then
	git submodule update --init --recursive
fi

rm -f disk.img
dd if=/dev/zero of=disk.img bs=4M count=$size

if [ "$fs" = "ext4" ]; then
	mkfs.ext4 -t ext4 disk.img
else if [ "$fs" = "fat32" ]; then
	fs=fat32
	mkfs.vfat -F 32 disk.img
else
	echo "Unknown filesystem: $fs"
	exit 1
fi
fi

mkdir -p mnt

#### 添加 MacOS 支持
os=`uname -s`
if [ "x$os" = "xDarwin" ];then
	hdiutil detach mnt > /dev/null 2>&1
	hdiutil attach disk.img -mountpoint mnt
	echo "Copying $arch $fs $FILE/* to disk"
	cp -r ./$FILE/* ./mnt/
	hdiutil detach mnt
	chmod 777 disk.img
else
	mount disk.img mnt
	# 根据命令行参数生成对应的测例
	echo "Copying $arch $fs $FILE/* to disk"
	cp -r ./$FILE/* ./mnt/
	umount mnt
	rm -rf mnt
	chmod 777 disk.img
fi