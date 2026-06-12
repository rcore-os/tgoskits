obj-m += linux-hello.o

# remove _mcount section
ccflags-remove-y += -pg
ccflags-remove-y += -fpatchable-function-entry=%

# StarryOS does not provide Linux x86 retbleed return thunk symbols. Host
# Kbuild enables this flag when CONFIG_RETHUNK=y, which makes the module emit
# jumps to __x86_return_thunk. Use normal function returns for this compatibility
# module so StarryOS can resolve all external symbols itself.
# ccflags-remove-y += -mfunction-return=thunk-extern

all:
	make -C /lib/modules/$(shell uname -r)/build M=$(PWD) modules
	strip -g  linux-hello.ko

clean:
	make -C /lib/modules/$(shell uname -r)/build M=$(PWD) clean
