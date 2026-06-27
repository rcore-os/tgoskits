DESTDIR ?=
PREFIX ?= /usr

TARGET := make-smoke
BUILD_DIR := build
SCRIPT := $(BUILD_DIR)/$(TARGET)
SOURCES := src/message.txt src/answer.txt

.PHONY: all clean install test

all: $(TARGET)

$(TARGET): $(SCRIPT)
	cp $(SCRIPT) $@
	chmod +x $@

$(SCRIPT): $(SOURCES)
	mkdir -p $(BUILD_DIR)
	{ \
		echo '#!/bin/sh'; \
		echo 'set -eu'; \
		printf 'printf "%s: %s\\n"\n' "$$(cat src/message.txt)" "$$(cat src/answer.txt)"; \
	} > $@
	chmod +x $@

test: $(TARGET)
	./$(TARGET) | grep -q "make-smoke: 42"

install: $(TARGET)
	install -Dm0755 $(TARGET) "$(DESTDIR)$(PREFIX)/bin/$(TARGET)"

clean:
	rm -rf $(TARGET) $(BUILD_DIR)
