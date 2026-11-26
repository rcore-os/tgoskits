SPDX-License-Identifier := Apache-2.0

.DEFAULT_GOAL := all

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
AXIVC_BENCH_PREPARED := $(or $(strip $(AXIVC_BENCH_PREPARED)),0)
ifneq ($(AXIVC_BENCH_PREPARED),0)
ifneq ($(AXIVC_BENCH_PREPARED),1)
$(error AXIVC_BENCH_PREPARED must be 0 or 1)
endif
endif

COMMON_DIR := $(abspath $(APP_DIR)/../../zephyr/ivshmem/samples/common)
COMMON_HEADER := $(COMMON_DIR)/tgos_ivc_app.h
IVC_DIR := $(abspath $(APP_DIR)/../../common/ivc/include)
IVC_HEADER := $(IVC_DIR)/ivc.h
DEVICE_HEADER := $(APP_DIR)/src/starry_axivc.h

CPPFLAGS += -D_POSIX_C_SOURCE=200809L
CPPFLAGS += -I$(APP_DIR)/src -I$(COMMON_DIR) -I$(IVC_DIR)
CPPFLAGS += -DAXIVC_BENCH_ITERATIONS=$(AXIVC_BENCH_ITERATIONS)
CFLAGS ?= -std=c11 -O2 -Wall -Wextra -Wpedantic -fno-common
LDFLAGS ?= -static -no-pie -Wl,--gc-sections

DEVICE_OBJ := $(OBJ_DIR)/starry_axivc.o
DEMO_OBJ := $(OBJ_DIR)/ivc_demo.o
BENCH_OBJ := $(OBJ_DIR)/ivc_starry_bench.o
DEMO_BIN := $(BUILD_DIR)/ivc-demo
BENCH_BIN := $(BUILD_DIR)/ivc-starry-bench

$(BENCH_OBJ): CPPFLAGS += -DAXIVC_BENCH_PREPARED=$(AXIVC_BENCH_PREPARED)
$(BENCH_OBJ): FORCE

.PHONY: all clean FORCE

all: $(DEMO_BIN) $(BENCH_BIN)

$(DEMO_BIN): $(DEMO_OBJ) $(DEVICE_OBJ)
	$(CC) $(LDFLAGS) -o $@ $^

$(BENCH_BIN): $(BENCH_OBJ) $(DEVICE_OBJ)
	$(CC) $(LDFLAGS) -o $@ $^

$(OBJ_DIR)/%.o: $(APP_DIR)/src/%.c $(COMMON_HEADER) $(IVC_HEADER) $(DEVICE_HEADER)
	@mkdir -p $(OBJ_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) -c $< -o $@

clean:
	rm -f "$(DEMO_BIN)" "$(BENCH_BIN)" "$(DEMO_OBJ)" "$(BENCH_OBJ)" "$(DEVICE_OBJ)"
	@rmdir "$(OBJ_DIR)" "$(BUILD_DIR)" 2>/dev/null || true
