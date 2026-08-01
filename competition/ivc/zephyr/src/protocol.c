/* SPDX-License-Identifier: Apache-2.0 */

#include "protocol.h"

#include <string.h>

#define IVC_CHECKSUM_OFFSET 28U

static const uint8_t ivc_magic[4] = {'I', 'V', 'C', '1'};

static uint16_t get_le16(const uint8_t *bytes)
{
	return (uint16_t)bytes[0] | ((uint16_t)bytes[1] << 8);
}

static uint32_t get_le32(const uint8_t *bytes)
{
	return (uint32_t)bytes[0] | ((uint32_t)bytes[1] << 8) |
	       ((uint32_t)bytes[2] << 16) | ((uint32_t)bytes[3] << 24);
}

static uint64_t get_le64(const uint8_t *bytes)
{
	return (uint64_t)get_le32(bytes) | ((uint64_t)get_le32(bytes + 4) << 32);
}

static void put_le16(uint8_t *bytes, uint16_t value)
{
	bytes[0] = (uint8_t)value;
	bytes[1] = (uint8_t)(value >> 8);
}

static void put_le32(uint8_t *bytes, uint32_t value)
{
	bytes[0] = (uint8_t)value;
	bytes[1] = (uint8_t)(value >> 8);
	bytes[2] = (uint8_t)(value >> 16);
	bytes[3] = (uint8_t)(value >> 24);
}

static void put_le64(uint8_t *bytes, uint64_t value)
{
	put_le32(bytes, (uint32_t)value);
	put_le32(bytes + 4, (uint32_t)(value >> 32));
}

static bool message_type_valid(uint8_t value)
{
	return value >= IVC_MESSAGE_CONTROL && value <= IVC_MESSAGE_TELEMETRY;
}

static bool error_code_valid(uint16_t value)
{
	return value <= IVC_ERROR_INTERNAL;
}

static bool mode_valid(uint8_t value)
{
	return value <= IVC_MODE_NEURAL;
}

static bool operation_valid(uint8_t value)
{
	return value >= IVC_CONTROL_SET_ACTUATOR && value <= IVC_CONTROL_HEARTBEAT;
}

static bool temperature_valid(int32_t value)
{
	return value >= IVC_MIN_TEMPERATURE_MILLI_C &&
	       value <= IVC_MAX_TEMPERATURE_MILLI_C;
}

static bool frame_error_code_valid(enum ivc_message_type message_type,
				   enum ivc_error_code error_code)
{
	if (!error_code_valid((uint16_t)error_code)) {
		return false;
	}
	if (message_type == IVC_MESSAGE_ERROR) {
		return error_code != IVC_ERROR_NONE;
	}
	return error_code == IVC_ERROR_NONE;
}

uint32_t ivc_crc32(const uint8_t *frame, size_t frame_length)
{
	uint32_t crc = UINT32_MAX;

	for (size_t index = 0; index < frame_length; ++index) {
		uint8_t byte = frame[index];

		if (index >= IVC_CHECKSUM_OFFSET && index < IVC_CHECKSUM_OFFSET + 4U) {
			byte = 0U;
		}
		crc ^= byte;
		for (unsigned int bit = 0; bit < 8U; ++bit) {
			uint32_t mask = (uint32_t)-(int32_t)(crc & 1U);

			crc = (crc >> 1) ^ (UINT32_C(0xedb88320) & mask);
		}
	}
	return ~crc;
}

bool ivc_encode_frame(const struct ivc_header *header, const uint8_t *payload,
		      uint8_t *frame, size_t frame_capacity, size_t *frame_length)
{
	size_t encoded_length;

	if (header == NULL || frame == NULL || frame_length == NULL ||
	    !message_type_valid((uint8_t)header->message_type) ||
	    (header->flags & ~IVC_VALID_FLAGS) != 0U ||
	    header->payload_length > IVC_MAX_PAYLOAD_LENGTH ||
	    !frame_error_code_valid(header->message_type, header->error_code) ||
	    (header->payload_length != 0U && payload == NULL)) {
		return false;
	}
	encoded_length = IVC_HEADER_LENGTH + header->payload_length;
	if (frame_capacity < encoded_length) {
		return false;
	}

	memcpy(frame, ivc_magic, sizeof(ivc_magic));
	frame[4] = IVC_PROTOCOL_VERSION;
	frame[5] = (uint8_t)header->message_type;
	put_le16(frame + 6, header->flags);
	put_le32(frame + 8, header->session_id);
	put_le32(frame + 12, header->sequence);
	put_le64(frame + 16, header->timestamp_us);
	put_le16(frame + 24, header->payload_length);
	put_le16(frame + 26, (uint16_t)header->error_code);
	put_le32(frame + IVC_CHECKSUM_OFFSET, 0U);
	if (header->payload_length != 0U) {
		memcpy(frame + IVC_HEADER_LENGTH, payload, header->payload_length);
	}
	put_le32(frame + IVC_CHECKSUM_OFFSET, ivc_crc32(frame, encoded_length));
	*frame_length = encoded_length;
	return true;
}

enum ivc_decode_result ivc_decode_frame(const uint8_t *frame, size_t frame_length,
					struct ivc_frame_view *view)
{
	uint8_t message_type;
	uint16_t flags;
	uint16_t payload_length;
	uint16_t error_code;

	if (frame == NULL || view == NULL || frame_length < IVC_HEADER_LENGTH) {
		return IVC_DECODE_TOO_SHORT;
	}
	if (memcmp(frame, ivc_magic, sizeof(ivc_magic)) != 0) {
		return IVC_DECODE_BAD_MAGIC;
	}
	if (frame[4] != IVC_PROTOCOL_VERSION) {
		return IVC_DECODE_UNSUPPORTED_VERSION;
	}
	message_type = frame[5];
	if (!message_type_valid(message_type)) {
		return IVC_DECODE_UNKNOWN_MESSAGE_TYPE;
	}
	flags = get_le16(frame + 6);
	if ((flags & ~IVC_VALID_FLAGS) != 0U) {
		return IVC_DECODE_INVALID_FLAGS;
	}
	payload_length = get_le16(frame + 24);
	if (payload_length > IVC_MAX_PAYLOAD_LENGTH) {
		return IVC_DECODE_PAYLOAD_TOO_LARGE;
	}
	if (frame_length != IVC_HEADER_LENGTH + payload_length) {
		return IVC_DECODE_LENGTH_MISMATCH;
	}
	error_code = get_le16(frame + 26);
	if (!frame_error_code_valid((enum ivc_message_type)message_type,
				    (enum ivc_error_code)error_code)) {
		return IVC_DECODE_INVALID_ERROR_CODE;
	}
	if (get_le32(frame + IVC_CHECKSUM_OFFSET) != ivc_crc32(frame, frame_length)) {
		return IVC_DECODE_CHECKSUM_MISMATCH;
	}

	view->header = (struct ivc_header){
		.message_type = (enum ivc_message_type)message_type,
		.flags = flags,
		.session_id = get_le32(frame + 8),
		.sequence = get_le32(frame + 12),
		.timestamp_us = get_le64(frame + 16),
		.payload_length = payload_length,
		.error_code = (enum ivc_error_code)error_code,
	};
	view->payload = frame + IVC_HEADER_LENGTH;
	return IVC_DECODE_OK;
}

const char *ivc_decode_result_name(enum ivc_decode_result result)
{
	switch (result) {
	case IVC_DECODE_OK:
		return "ok";
	case IVC_DECODE_TOO_SHORT:
		return "frame-too-short";
	case IVC_DECODE_BAD_MAGIC:
		return "bad-magic";
	case IVC_DECODE_UNSUPPORTED_VERSION:
		return "unsupported-version";
	case IVC_DECODE_UNKNOWN_MESSAGE_TYPE:
		return "unknown-message-type";
	case IVC_DECODE_INVALID_FLAGS:
		return "invalid-flags";
	case IVC_DECODE_PAYLOAD_TOO_LARGE:
		return "payload-too-large";
	case IVC_DECODE_LENGTH_MISMATCH:
		return "length-mismatch";
	case IVC_DECODE_INVALID_ERROR_CODE:
		return "invalid-error-code";
	case IVC_DECODE_CHECKSUM_MISMATCH:
		return "checksum-mismatch";
	default:
		return "unknown-decode-result";
	}
}

bool ivc_encode_control(const struct ivc_control_command *command,
			uint8_t payload[IVC_CONTROL_PAYLOAD_LENGTH])
{
	if (command == NULL || payload == NULL ||
	    !operation_valid((uint8_t)command->operation) || !mode_valid((uint8_t)command->mode) ||
	    command->actuator_permille > IVC_MAX_ACTUATOR_PERMILLE ||
	    !temperature_valid(command->setpoint_milli_c) ||
	    (command->operation == IVC_CONTROL_ENTER_SAFE_STATE && command->mode != IVC_MODE_SAFE)) {
		return false;
	}
	payload[0] = (uint8_t)command->operation;
	payload[1] = (uint8_t)command->mode;
	put_le16(payload + 2, command->actuator_permille);
	put_le32(payload + 4, (uint32_t)command->setpoint_milli_c);
	put_le32(payload + 8, command->sample_id);
	return true;
}

bool ivc_decode_control(const uint8_t *payload, size_t payload_length,
			struct ivc_control_command *command)
{
	struct ivc_control_command decoded;

	if (payload == NULL || command == NULL || payload_length != IVC_CONTROL_PAYLOAD_LENGTH) {
		return false;
	}
	decoded = (struct ivc_control_command){
		.operation = (enum ivc_control_operation)payload[0],
		.mode = (enum ivc_control_mode)payload[1],
		.actuator_permille = get_le16(payload + 2),
		.setpoint_milli_c = (int32_t)get_le32(payload + 4),
		.sample_id = get_le32(payload + 8),
	};
	if (!operation_valid(payload[0]) || !mode_valid(payload[1]) ||
	    decoded.actuator_permille > IVC_MAX_ACTUATOR_PERMILLE ||
	    !temperature_valid(decoded.setpoint_milli_c) ||
	    (decoded.operation == IVC_CONTROL_ENTER_SAFE_STATE && decoded.mode != IVC_MODE_SAFE)) {
		return false;
	}
	*command = decoded;
	return true;
}

bool ivc_encode_status(const struct ivc_status_report *status,
		       uint8_t payload[IVC_STATUS_PAYLOAD_LENGTH])
{
	bool fault_state;

	if (status == NULL || payload == NULL || status->state < IVC_STATUS_READY ||
	    status->state > IVC_STATUS_FAULT || !mode_valid((uint8_t)status->active_mode) ||
	    status->actuator_permille > IVC_MAX_ACTUATOR_PERMILLE ||
	    !temperature_valid(status->measured_milli_c) ||
	    !temperature_valid(status->setpoint_milli_c) ||
	    !error_code_valid((uint16_t)status->fault)) {
		return false;
	}
	fault_state = status->state == IVC_STATUS_FAULT;
	if ((fault_state && status->fault == IVC_ERROR_NONE) ||
	    (!fault_state && status->state != IVC_STATUS_SAFE_FALLBACK &&
	     status->fault != IVC_ERROR_NONE)) {
		return false;
	}
	payload[0] = (uint8_t)status->state;
	payload[1] = (uint8_t)status->active_mode;
	put_le16(payload + 2, status->actuator_permille);
	put_le32(payload + 4, (uint32_t)status->measured_milli_c);
	put_le32(payload + 8, (uint32_t)status->setpoint_milli_c);
	put_le32(payload + 12, status->applied_sequence);
	put_le16(payload + 16, (uint16_t)status->fault);
	put_le16(payload + 18, 0U);
	return true;
}

bool ivc_encode_ack(const struct ivc_ack_payload *ack,
		    uint8_t payload[IVC_ACK_PAYLOAD_LENGTH])
{
	if (ack == NULL || payload == NULL) {
		return false;
	}
	put_le32(payload, ack->acknowledged_sequence);
	put_le32(payload + 4, ack->next_expected_sequence);
	put_le32(payload + 8, ack->received_mask);
	return true;
}

bool ivc_encode_error_payload(const struct ivc_error_payload *error,
			      uint8_t payload[IVC_ERROR_PAYLOAD_LENGTH])
{
	if (error == NULL || payload == NULL ||
	    !message_type_valid((uint8_t)error->offending_message_type)) {
		return false;
	}
	payload[0] = (uint8_t)error->offending_message_type;
	payload[1] = 0U;
	payload[2] = 0U;
	payload[3] = 0U;
	put_le32(payload + 4, error->offending_sequence);
	return true;
}

bool ivc_protocol_self_test(void)
{
	static const uint8_t expected_frame[] = {
		0x49, 0x56, 0x43, 0x31, 0x01, 0x01, 0x01, 0x00,
		0x04, 0x03, 0x02, 0x01, 0x08, 0x07, 0x06, 0x05,
		0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11,
		0x02, 0x00, 0x00, 0x00, 0xea, 0x5d, 0x15, 0xfe,
		0xaa, 0x55,
	};
	static const uint8_t expected_control[] = {
		0x01, 0x02, 0x71, 0x02, 0xd8, 0xd6,
		0x00, 0x00, 0x04, 0x03, 0x02, 0x01,
	};
	const struct ivc_header header = {
		.message_type = IVC_MESSAGE_CONTROL,
		.flags = IVC_FLAG_ACK_REQUIRED,
		.session_id = UINT32_C(0x01020304),
		.sequence = UINT32_C(0x05060708),
		.timestamp_us = UINT64_C(0x1112131415161718),
		.payload_length = 2U,
		.error_code = IVC_ERROR_NONE,
	};
	const struct ivc_control_command command = {
		.operation = IVC_CONTROL_SET_ACTUATOR,
		.mode = IVC_MODE_NEURAL,
		.actuator_permille = 625U,
		.setpoint_milli_c = 55000,
		.sample_id = UINT32_C(0x01020304),
	};
	const uint8_t frame_payload[] = {0xaa, 0x55};
	uint8_t frame[sizeof(expected_frame)];
	uint8_t control[sizeof(expected_control)];
	struct ivc_frame_view view;
	struct ivc_control_command decoded_command;
	size_t frame_length = 0U;

	return ivc_encode_frame(&header, frame_payload, frame, sizeof(frame), &frame_length) &&
	       frame_length == sizeof(expected_frame) &&
	       memcmp(frame, expected_frame, sizeof(expected_frame)) == 0 &&
	       ivc_decode_frame(frame, frame_length, &view) == IVC_DECODE_OK &&
	       view.header.session_id == header.session_id &&
	       view.header.sequence == header.sequence &&
	       view.header.timestamp_us == header.timestamp_us &&
	       memcmp(view.payload, frame_payload, sizeof(frame_payload)) == 0 &&
	       ivc_encode_control(&command, control) &&
	       memcmp(control, expected_control, sizeof(expected_control)) == 0 &&
	       ivc_decode_control(control, sizeof(control), &decoded_command) &&
	       decoded_command.operation == command.operation && decoded_command.mode == command.mode &&
	       decoded_command.actuator_permille == command.actuator_permille &&
	       decoded_command.setpoint_milli_c == command.setpoint_milli_c &&
	       decoded_command.sample_id == command.sample_id;
}
