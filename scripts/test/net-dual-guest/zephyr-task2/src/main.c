/*
 * Zephyr T2N1 managed endpoint for the task-2 dual-Guest experiment.
 *
 * The wire format intentionally mirrors components/task2-net-protocol.  This
 * adapter owns only UDP and the endpoint state; the virtio driver and network
 * stack remain Zephyr responsibilities.
 */

#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <zephyr/kernel.h>
#include <zephyr/net/net_if.h>
#include <zephyr/net/net_ip.h>
#include <zephyr/net/socket.h>

#define UDP_PORT 4242
#define SESSION_ID UINT32_C(0x54525432)
#define FRAME_HEADER_LEN 28
#define MAX_PAYLOAD_LEN 1200
#define MAX_DATAGRAM_LEN (FRAME_HEADER_LEN + MAX_PAYLOAD_LEN)
#define RETRANSMIT_AFTER_MS 500
#define MAX_RETRIES 5
#define HEARTBEAT_INTERVAL_MS 200
#define PEER_TIMEOUT_MS 10000

/*
 * Task-3 virtual plant: a nonlinear thermal-like object in 0..1000.
 *
 *   loss(state) = base_loss + nonlinear_loss * state^2 / 1_000_000
 *   state_next  = clamp(state + response * (output - state)
 *                       - loss(state) + disturbance, 0, 1000)
 *
 * The plant runs inside the RTOS Guest; the Linux side only observes it
 * through the T2N1 STATUS value.  Parameters are frozen (see
 * book/design/task3-ai-control-todo.md M0) and must not be tuned after
 * baseline/AI comparison runs start.
 */
#define PLANT_STATE_MIN 0
#define PLANT_STATE_MAX 1000
#define PLANT_BASE_LOSS 15
#define PLANT_NONLINEAR_LOSS 120
#define PLANT_RESPONSE 350
#define PLANT_RESPONSE_DEN 1000
#define PLANT_DISTURBANCE_MAX 150

static int32_t plant_state = 300;
static int32_t plant_disturbance = 0;

static int task2_configure_network(void)
{
	struct net_if *iface = net_if_get_default();
	struct net_in_addr local;
	struct net_in_addr netmask;

	if (iface == NULL || net_addr_pton(NET_AF_INET, "10.0.42.2", &local) != 0 ||
	    net_addr_pton(NET_AF_INET, "255.255.255.0", &netmask) != 0) {
		return -EINVAL;
	}

	if (net_if_ipv4_addr_add(iface, &local, NET_ADDR_MANUAL, 0) == NULL ||
	    !net_if_ipv4_set_netmask_by_addr(iface, &local, &netmask)) {
		return -EINVAL;
	}

	int status = net_if_up(iface);
	return status < 0 && status != -EALREADY ? status : 0;
}

enum message_kind {
	KIND_CONTROL = 1,
	KIND_STATUS = 2,
	KIND_ERROR = 3,
	KIND_ACK = 4,
	KIND_HEARTBEAT = 5,
};

enum error_code {
	ERROR_INVALID_PARAMETER = 1,
	ERROR_OUT_OF_ORDER = 2,
	ERROR_UNSUPPORTED_MESSAGE = 3,
	ERROR_SESSION_MISMATCH = 4,
};

static uint32_t read_u32(const uint8_t *bytes)
{
	return ((uint32_t)bytes[0] << 24) | ((uint32_t)bytes[1] << 16) |
	       ((uint32_t)bytes[2] << 8) | bytes[3];
}

static uint16_t read_u16(const uint8_t *bytes)
{
	return (uint16_t)(((uint16_t)bytes[0] << 8) | bytes[1]);
}

static void write_u32(uint8_t *bytes, uint32_t value)
{
	bytes[0] = (uint8_t)(value >> 24);
	bytes[1] = (uint8_t)(value >> 16);
	bytes[2] = (uint8_t)(value >> 8);
	bytes[3] = (uint8_t)value;
}

static void write_u16(uint8_t *bytes, uint16_t value)
{
	bytes[0] = (uint8_t)(value >> 8);
	bytes[1] = (uint8_t)value;
}

static uint32_t crc32_update(uint32_t crc, const uint8_t *bytes, size_t length)
{
	for (size_t index = 0; index < length; index++) {
		crc ^= bytes[index];
		for (unsigned bit = 0; bit < 8; bit++) {
			crc = (crc >> 1) ^ (UINT32_C(0xedb88320) & (-(int32_t)(crc & 1)));
		}
	}
	return crc;
}

static uint32_t frame_crc(const uint8_t *frame, size_t length)
{
	uint32_t crc = UINT32_MAX;

	crc = crc32_update(crc, frame, 24);
	uint8_t zero_checksum[4] = {0};
	crc = crc32_update(crc, zero_checksum, sizeof(zero_checksum));
	crc = crc32_update(crc, frame + 28, length - 28);
	return ~crc;
}

static size_t encode_frame(uint8_t *output, uint8_t kind, uint16_t flags,
				   uint32_t sequence, uint32_t acknowledgement,
				   uint16_t error, const uint8_t *payload, size_t payload_len)
{
	if (payload_len > MAX_PAYLOAD_LEN) {
		return 0;
	}
	memset(output, 0, FRAME_HEADER_LEN + payload_len);
	memcpy(output, "T2N1", 4);
	output[4] = 1;
	output[5] = kind;
	write_u16(output + 6, flags);
	write_u32(output + 8, SESSION_ID);
	write_u32(output + 12, sequence);
	write_u32(output + 16, acknowledgement);
	write_u16(output + 20, (uint16_t)payload_len);
	write_u16(output + 22, error);
	memcpy(output + FRAME_HEADER_LEN, payload, payload_len);
	write_u32(output + 24, frame_crc(output, FRAME_HEADER_LEN + payload_len));
	return FRAME_HEADER_LEN + payload_len;
}

static int parse_frame(const uint8_t *frame, size_t length, uint8_t *kind,
			       uint16_t *flags, uint32_t *sequence,
			       uint32_t *acknowledgement, uint16_t *error,
			       const uint8_t **payload, size_t *payload_len)
{
	if (length < FRAME_HEADER_LEN || memcmp(frame, "T2N1", 4) != 0 || frame[4] != 1) {
		return -EINVAL;
	}
	*kind = frame[5];
	*flags = read_u16(frame + 6);
	*sequence = read_u32(frame + 12);
	*acknowledgement = read_u32(frame + 16);
	*payload_len = read_u16(frame + 20);
	*error = read_u16(frame + 22);
	if ((*flags & ~UINT16_C(1)) != 0 || *payload_len > MAX_PAYLOAD_LEN ||
	    *payload_len != length - FRAME_HEADER_LEN || read_u32(frame + 8) != SESSION_ID ||
	    read_u32(frame + 24) != frame_crc(frame, length)) {
		return -EINVAL;
	}
	*payload = frame + FRAME_HEADER_LEN;
	if ((*kind == KIND_CONTROL || *kind == KIND_STATUS) &&
	    (*flags != 1 || *sequence == 0 || *acknowledgement != 0 || *error != 0)) {
		return -EINVAL;
	}
	if (*kind == KIND_ACK &&
	    (*flags != 0 || *sequence != 0 || *acknowledgement == 0 || *error != 0 ||
	     *payload_len != 0)) {
		return -EINVAL;
	}
	if (*kind == KIND_ERROR &&
	    (*flags != 0 || *sequence != 0 || *acknowledgement == 0 || *error == 0)) {
		return -EINVAL;
	}
	if (*kind == KIND_HEARTBEAT &&
	    (*flags != 0 || *sequence != 0 || *acknowledgement != 0 || *error != 0)) {
		return -EINVAL;
	}
	if (*kind < KIND_CONTROL || *kind > KIND_HEARTBEAT) {
		return -EINVAL;
	}
	return 0;
}

static bool valid_control(const uint8_t *payload, size_t length)
{
	if (length != 12 || payload[1] != 0 || payload[2] != 0 || payload[3] != 0) {
		return false;
	}
	uint8_t action = payload[0];
	int32_t value = (int32_t)read_u32(payload + 4);
	if (action == 1) {
		return value >= 0 && value <= 1000;
	}
	return (action == 2 || action == 3) && value == 0;
}

static bool valid_status(const uint8_t *payload, size_t length)
{
	return length == 12 && payload[0] >= 1 && payload[0] <= 3 &&
	       (payload[1] & ~UINT8_C(3)) == 0 && payload[2] == 0 && payload[3] == 0;
}

static bool valid_heartbeat(const uint8_t *payload, size_t length)
{
	return length == 8;
}

static int send_frame(int socket, const struct sockaddr_in *peer, uint8_t *frame,
			      size_t length)
{
	return zsock_sendto(socket, frame, length, 0, (const struct sockaddr *)peer,
			    sizeof(*peer));
}

static size_t make_ack(uint8_t *frame, uint32_t sequence)
{
	return encode_frame(frame, KIND_ACK, 0, 0, sequence, 0, NULL, 0);
}

static size_t make_error(uint8_t *frame, uint32_t sequence, uint16_t error)
{
	return encode_frame(frame, KIND_ERROR, 0, 0, sequence, error, NULL, 0);
}

static size_t make_heartbeat(uint8_t *frame, uint64_t uptime_ms)
{
	uint8_t payload[8];
	for (unsigned index = 0; index < 8; index++) {
		payload[7 - index] = (uint8_t)(uptime_ms >> (index * 8));
	}
	return encode_frame(frame, KIND_HEARTBEAT, 0, 0, 0, 0, payload, sizeof(payload));
}

/*
 * Fixed disturbance schedule (frozen in M0):
 *   8 s  after boot: +150 load step
 *   17 s after boot: -150 load step
 */
static void plant_update_disturbance(int64_t now_ms)
{
	int64_t scheduled = 0;

	if (now_ms >= 8000 && now_ms < 17000) {
		scheduled = PLANT_DISTURBANCE_MAX;
	} else if (now_ms >= 17000) {
		scheduled = 0;
	}
	if (scheduled != plant_disturbance) {
		plant_disturbance = scheduled;
		printk("TASK3_DISTURBANCE change=%d at_ms=%lld\n", (int)scheduled, (long long)now_ms);
	}
}

static int32_t plant_update(int32_t output)
{
	int64_t loss = PLANT_BASE_LOSS +
		       (int64_t)PLANT_NONLINEAR_LOSS * plant_state * plant_state / 1000000;
	int64_t response = (int64_t)PLANT_RESPONSE * (output - plant_state) / PLANT_RESPONSE_DEN;
	int64_t next = plant_state + response - loss + plant_disturbance;

	if (next < PLANT_STATE_MIN) {
		next = PLANT_STATE_MIN;
	} else if (next > PLANT_STATE_MAX) {
		next = PLANT_STATE_MAX;
	}
	plant_state = (int32_t)next;
	return plant_state;
}

int main(void)
{
	struct sockaddr_in local = {0};
	struct sockaddr_in peer = {0};
	uint8_t inbound[MAX_DATAGRAM_LEN];
	uint8_t outbound[MAX_DATAGRAM_LEN];
	uint8_t pending_frame[MAX_DATAGRAM_LEN];
	size_t pending_length = 0;
	uint32_t next_tx_sequence = 1;
	uint32_t expected_rx_sequence = 1;
	uint32_t last_acknowledged = 0;
	uint32_t pending_sequence = 0;
	uint8_t retry_count = 0;
	int state_safe = 0;
	int socket;
	int64_t now;
	int64_t last_rx = k_uptime_get();
	int64_t last_tx = last_rx;
	int64_t pending_sent = last_rx;

	printk("TASK2_MAIN_START\n");
	if (task2_configure_network() != 0) {
		printk("TASK2_ERROR network configuration failed\n");
		return 1;
	}
	local.sin_family = AF_INET;
	local.sin_port = htons(UDP_PORT);
	local.sin_addr.s_addr = htonl(INADDR_ANY);
	peer.sin_family = AF_INET;
	peer.sin_port = htons(UDP_PORT);
	if (zsock_inet_pton(AF_INET, "10.0.42.15", &peer.sin_addr) != 1) {
		printk("TASK2_ERROR invalid peer address\n");
		return 1;
	}
	printk("TASK2_SOCKET_CREATE\n");
	socket = zsock_socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
	printk("TASK2_SOCKET_CREATED fd=%d\n", socket);
	if (socket < 0 || zsock_bind(socket, (struct sockaddr *)&local, sizeof(local)) < 0) {
		printk("TASK2_ERROR UDP bind errno=%d\n", errno);
		return 1;
	}
	printk("TASK2_NET_CONFIGURED interface=virtio-net ip=10.0.42.2/24\n");
	printk("TASK2_READY role=managed local=10.0.42.2:4242 peer=10.0.42.15:4242\n");

	for (;;) {
		struct zsock_pollfd pollfd = {.fd = socket, .events = ZSOCK_POLLIN};
		int poll_result = zsock_poll(&pollfd, 1, 100);
		now = k_uptime_get();
		if (poll_result > 0 && (pollfd.revents & ZSOCK_POLLIN) != 0) {
			struct sockaddr_in source;
			socklen_t source_length = sizeof(source);
			ssize_t received = zsock_recvfrom(socket, inbound, sizeof(inbound), 0,
							(struct sockaddr *)&source, &source_length);
			uint8_t kind;
			uint16_t flags, error;
			uint32_t sequence, acknowledgement;
			const uint8_t *payload;
			size_t payload_length;
			if (received < 0 || parse_frame(inbound, (size_t)received, &kind, &flags,
							       &sequence, &acknowledgement, &error, &payload,
							       &payload_length) != 0) {
				printk("TASK2_REJECTED malformed_frame\n");
			} else {
				last_rx = now;
				int was_safe = state_safe;
				state_safe = 0;
				if (was_safe) {
					/* Mirror the Rust endpoint's recovery resync: after a
					 * data-link outage both sides restart the reliable
					 * stream from sequence 1.  Otherwise a lost STATUS
					 * (the peer advanced to n+1 while we still expect n)
					 * loops on OutOfOrder forever after recovery. */
					next_tx_sequence = 1;
					expected_rx_sequence = 1;
					last_acknowledged = 0;
					pending_sequence = 0;
					pending_length = 0;
					retry_count = 0;
				}
				if (kind == KIND_ACK) {
					if (pending_length > 0 && acknowledgement == pending_sequence) {
						pending_length = 0;
						last_acknowledged = acknowledgement;
						printk("TASK2_ACK seq=%u\n", acknowledgement);
					} else if (acknowledgement == last_acknowledged) {
						printk("TASK2_DUPLICATE_ACK seq=%u\n", acknowledgement);
					}
				} else if (kind == KIND_HEARTBEAT) {
					if (!valid_heartbeat(payload, payload_length)) {
						size_t error_length = make_error(outbound, 0,
											ERROR_INVALID_PARAMETER);
						(void)send_frame(socket, &source, outbound, error_length);
						state_safe = 1;
						printk("TASK2_SAFE state=Safe event=InvalidParameter\n");
					} else {
						uint64_t uptime = 0;
						for (unsigned index = 0; index < 8; index++) {
							uptime = (uptime << 8) | payload[index];
						}
						printk("TASK2_HEARTBEAT_RECEIVED peer_uptime_ms=%llu\n", uptime);
					}
				} else if (kind == KIND_CONTROL || kind == KIND_STATUS) {
					bool valid = kind == KIND_CONTROL ? valid_control(payload, payload_length)
											 : valid_status(payload, payload_length);
					if (!valid) {
						size_t error_length = make_error(outbound, sequence,
											ERROR_INVALID_PARAMETER);
						(void)send_frame(socket, &source, outbound, error_length);
						state_safe = 1;
						printk("TASK2_PROTOCOL_ERROR invalid_parameter seq=%u\n", sequence);
					} else if (sequence == expected_rx_sequence) {
						expected_rx_sequence = sequence == UINT32_MAX ? 1 : sequence + 1;
						size_t ack_length = make_ack(outbound, sequence);
						(void)send_frame(socket, &source, outbound, ack_length);
						if (kind == KIND_CONTROL) {
						int32_t output = (int32_t)read_u32(payload + 4);
						uint32_t request = read_u32(payload + 8);
						int32_t state_before = plant_state;

						plant_update_disturbance(now);
						if (output >= PLANT_STATE_MIN && output <= PLANT_STATE_MAX) {
							plant_update(output);
						} else {
							/* Bounded values were validated on the wire;
							 * keep a defensive clamp for the plant only. */
							plant_update(output < PLANT_STATE_MIN ? PLANT_STATE_MIN :
								       output > PLANT_STATE_MAX ? PLANT_STATE_MAX :
								       output);
						}

						uint8_t status_payload[12] = {1, 0, 0, 0};
						write_u32(status_payload + 4, (uint32_t)plant_state);
						write_u32(status_payload + 8, request);
						pending_sequence = next_tx_sequence++;
						if (pending_sequence == 0) {
							pending_sequence = next_tx_sequence++;
						}
						pending_length = encode_frame(pending_frame, KIND_STATUS, 1,
											pending_sequence, 0, 0, status_payload, 12);
						pending_sent = now;
						retry_count = 0;
						(void)send_frame(socket, &source, pending_frame, pending_length);
						last_tx = now;
						printk("TASK2_CONTROL_RECEIVED seq=%u request=%u\n", sequence,
						       request);
						printk("TASK3_CONTROL_APPLIED request=%u value=%d\n", request,
						       output);
						printk("TASK3_PLANT_STATE before=%d after=%d dist=%d\n",
						       state_before, plant_state, plant_disturbance);
						printk("TASK2_STATUS_SENT seq=%u\n", pending_sequence);
					}
					} else if (sequence == (expected_rx_sequence == 1 ? UINT32_MAX : expected_rx_sequence - 1)) {
						size_t ack_length = make_ack(outbound, sequence);
						(void)send_frame(socket, &source, outbound, ack_length);
						printk("TASK2_DUPLICATE seq=%u\n", sequence);
					} else {
						size_t error_length = make_error(outbound, sequence, ERROR_OUT_OF_ORDER);
						(void)send_frame(socket, &source, outbound, error_length);
						state_safe = 1;
						printk("TASK2_PROTOCOL_ERROR out_of_order=%u expected=%u\n", sequence,
						       expected_rx_sequence);
					}
				} else {
					printk("TASK2_REJECTED unsupported_kind=%u\n", kind);
				}
			}
		}

		if (pending_length > 0 && now - pending_sent >= RETRANSMIT_AFTER_MS) {
			if (retry_count >= MAX_RETRIES) {
				pending_length = 0;
				state_safe = 1;
				printk("TASK2_SAFE state=Safe event=RetryExhausted seq=%u\n", pending_sequence);
			} else {
				retry_count++;
				pending_sent = now;
				(void)send_frame(socket, &peer, pending_frame, pending_length);
				printk("TASK2_RETRANSMIT seq=%u attempt=%u\n", pending_sequence,
				       retry_count);
			}
		}
		if (!state_safe && now - last_rx >= PEER_TIMEOUT_MS) {
			state_safe = 1;
			printk("TASK2_SAFE state=Safe event=HeartbeatTimeout\n");
		}
		if (now - last_tx >= HEARTBEAT_INTERVAL_MS) {
			size_t heartbeat_length = make_heartbeat(outbound, (uint64_t)now);
			(void)send_frame(socket, &peer, outbound, heartbeat_length);
			last_tx = now;
		}
	}
}
