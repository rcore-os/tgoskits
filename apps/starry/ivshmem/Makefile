SPDX-License-Identifier := Apache-2.0

APP_DIR := $(patsubst %/,%,$(dir $(abspath $(lastword $(MAKEFILE_LIST)))))
BUILD_DIR := $(strip $(BUILD_DIR))
ifeq ($(BUILD_DIR),)
BUILD_DIR := $(APP_DIR)/build
endif
OBJ_DIR := $(BUILD_DIR)/obj

ifeq ($(origin CC),default)
CC := aarch64-linux-musl-gcc
endif
AXIVC_BENCH_ITERATIONS := $(or $(strip $(AXIVC_BENCH_ITERATIONS)),100)

COMMON_DIR := $(abspath $(APP_DIR)/../../zephyr/ivshmem/samples/common)
COMMON_HEADER := $(COMMON_DIR)/tgos_ivc_app.h
DEVICE_HEADER := $(APP_DIR)/src/starry_axivc.h

CPPFLAGS += -D_POSIX_C_SOURCE=200809L
CPPFLAGS += -I$(APP_DIR)/src -I$(COMMON_DIR)
CPPFLAGS += -DAXIVC_BENCH_ITERATIONS=$(AXIVC_BENCH_ITERATIONS)
CFLAGS ?= -std=c11 -O2 -Wall -Wextra -Wpedantic -fno-common
LDFLAGS ?= -static -no-pie -Wl,--gc-sections

DEVICE_OBJ := $(OBJ_DIR)/starry_axivc.o
DEMO_OBJ := $(OBJ_DIR)/ivc_demo.o
BENCH_OBJ := $(OBJ_DIR)/ivc_starry_bench.o
DEMO_BIN := $(BUILD_DIR)/ivc-demo
BENCH_BIN := $(BUILD_DIR)/ivc-starry-bench

.PHONY: all clean

all: $(DEMO_BIN) $(BENCH_BIN)

$(DEMO_BIN): $(DEMO_OBJ) $(DEVICE_OBJ)
	$(CC) $(LDFLAGS) -o $@ $^

$(BENCH_BIN): $(BENCH_OBJ) $(DEVICE_OBJ)
	$(CC) $(LDFLAGS) -o $@ $^

$(OBJ_DIR)/%.o: $(APP_DIR)/src/%.c $(COMMON_HEADER) $(DEVICE_HEADER)
	@mkdir -p $(OBJ_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) -c $< -o $@

clean:
	rm -f "$(DEMO_BIN)" "$(BENCH_BIN)" "$(DEMO_OBJ)" "$(BENCH_OBJ)" "$(DEVICE_OBJ)"
	@rmdir "$(OBJ_DIR)" "$(BUILD_DIR)" 2>/dev/null || true
