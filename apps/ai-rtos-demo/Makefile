CC ?= cc
CFLAGS ?= -O2 -g -Wall -Wextra -Werror -std=c11
CPPFLAGS += -D_POSIX_C_SOURCE=200809L -I./aicp -I./rtos-core
LDLIBS += -lm

BIN_DIR ?= build
PROTO_TEST := $(BIN_DIR)/aicp_protocol_test
SERVICE_TEST := $(BIN_DIR)/aicp_service_test
CLIENT_TEST := $(BIN_DIR)/aicp_client_test
REFERENCE_SERVER := $(BIN_DIR)/aicp_reference_server
REFERENCE_SERVER_SMOKE := $(BIN_DIR)/aicp_reference_server_smoke

AICP_PROTOCOL_HEADERS := aicp/aicp.h aicp/aicp_stream.h aicp/aicp_datagram.h
AICP_POSIX_HEADERS := $(AICP_PROTOCOL_HEADERS) aicp/aicp_posix_stream.h
AICP_SERVICE_CORE := rtos-core/aicp_service.c rtos-core/control_loop.c
AICP_SERVICE_HEADERS := rtos-core/aicp_service.h rtos-core/control_loop.h

.PHONY: protocol-test service-test client-test reference-server-smoke test reference-server clean

$(BIN_DIR):
	mkdir -p $@

$(PROTO_TEST): tests/aicp_protocol_test.c $(AICP_POSIX_HEADERS) | $(BIN_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) -o $@ $<

$(SERVICE_TEST): tests/aicp_service_test.c $(AICP_SERVICE_CORE) $(AICP_SERVICE_HEADERS) $(AICP_POSIX_HEADERS) | $(BIN_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) -pthread -o $@ tests/aicp_service_test.c $(AICP_SERVICE_CORE) $(LDLIBS)

$(CLIENT_TEST): tests/aicp_client_test.c aicp/aicp_client.c aicp/aicp_client.h $(AICP_POSIX_HEADERS) | $(BIN_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) -pthread -o $@ tests/aicp_client_test.c aicp/aicp_client.c

$(REFERENCE_SERVER): host-tools/aicp_reference_server.c $(AICP_SERVICE_CORE) $(AICP_SERVICE_HEADERS) $(AICP_POSIX_HEADERS) | $(BIN_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) -o $@ host-tools/aicp_reference_server.c $(AICP_SERVICE_CORE) $(LDLIBS)

$(REFERENCE_SERVER_SMOKE): tests/aicp_reference_server_smoke.c aicp/aicp_client.c aicp/aicp_client.h $(AICP_POSIX_HEADERS) | $(BIN_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) -o $@ tests/aicp_reference_server_smoke.c aicp/aicp_client.c

protocol-test: $(PROTO_TEST)
	$(PROTO_TEST)

service-test: $(SERVICE_TEST)
	$(SERVICE_TEST)

client-test: $(CLIENT_TEST)
	$(CLIENT_TEST)

reference-server-smoke: $(REFERENCE_SERVER) $(REFERENCE_SERVER_SMOKE)
	$(REFERENCE_SERVER_SMOKE) $(REFERENCE_SERVER)

test: protocol-test service-test client-test reference-server-smoke

reference-server: $(REFERENCE_SERVER)

clean:
	rm -rf $(BIN_DIR)
