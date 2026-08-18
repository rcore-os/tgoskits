# FreeRTOS VTP guest — driver / netif / agent objects.
#
# This builds the VTP-specific objects; the caller links them with their
# FreeRTOS kernel + lwIP sources and the platform port (startup, UART
# retargeting, GIC, timer).
#
# Required variables:
#   TGOSKITS_DIR   path to the tgoskits checkout (defaults to repo root)
#   FREERTOS_INC   FreeRTOS header dir(s), e.g. the kernel root + portable include
#   LWIP_INC       lwIP include dirs: -I<lwip>/src/include plus any port dir
#   CROSS          cross prefix (default aarch64-none-elf-)
#
# Example:
#   make TGOSKITS_DIR=/path/to/tgoskits \
#        "FREERTOS_INC=-I<rtos>/include" \
#        "LWIP_INC=-I<lwip>/src/include" \
#        CROSS=aarch64-none-elf-

TGOSKITS_DIR ?= ../../..
CROSS       ?= aarch64-none-elf-

CC      := $(CROSS)gcc
AR      := $(CROSS)ar

BUILD   := build
VTP_PROTOCOL := $(TGOSKITS_DIR)/test-suit/axvisor/normal/qemu-vtp/protocol

INCLUDES := -Isrc $(FREERTOS_INC) $(LWIP_INC) -I$(VTP_PROTOCOL)

# Cortex-A53, no ABI soft-float on aarch64. -ffreestanding keeps the objects
# usable inside a bare-metal RTOS. -fno-strict-aliasing guards the packed
# ring struct access.
CFLAGS := -mcpu=cortex-a53 -O2 -g -Wall -Wextra -Werror -std=c11 \
          -ffreestanding -fno-strict-aliasing $(CFLAGS_EXTRA)

OBJS := $(BUILD)/virtio_mmio.o \
        $(BUILD)/virtio_net.o \
        $(BUILD)/lwip_netif.o \
        $(BUILD)/vtp_agent.o \
        $(BUILD)/vtp.o

all: $(BUILD)/libvtp.a

$(BUILD):
	mkdir -p $(BUILD)

$(BUILD)/%.o: src/%.c | $(BUILD)
	$(CC) $(CFLAGS) $(INCLUDES) -c $< -o $@

$(BUILD)/vtp.o: $(VTP_PROTOCOL)/vtp.c | $(BUILD)
	$(CC) $(CFLAGS) $(INCLUDES) -c $< -o $@

$(BUILD)/libvtp.a: $(OBJS)
	$(AR) rcs $@ $^

clean:
	rm -rf $(BUILD)

.PHONY: all clean
