#define _GNU_SOURCE

#include <arpa/inet.h>
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static void fail(const char *step)
{
	printf("STARRY_UDP_FAIL step=%s errno=%d (%s)\n", step, errno,
	       strerror(errno));
	fflush(stdout);
}

int main(void)
{
	static const char request[] = "STARRY_UDP_PROBE_V1";
	char response[128] = {0};
	struct sockaddr_in local = {0};
	struct sockaddr_in peer = {0};
	struct pollfd pfd = {0};
	socklen_t peer_len = sizeof(peer);

	int fd = socket(AF_INET, SOCK_DGRAM, 0);
	if (fd < 0) {
		fail("socket");
		return 1;
	}
	printf("STARRY_UDP_SOCKET_READY fd=%d\n", fd);

	local.sin_family = AF_INET;
	local.sin_addr.s_addr = htonl(INADDR_ANY);
	local.sin_port = htons(0);
	if (bind(fd, (struct sockaddr *)&local, sizeof(local)) < 0) {
		fail("bind");
		close(fd);
		return 1;
	}
	printf("STARRY_UDP_BIND_OK\n");

	peer.sin_family = AF_INET;
	peer.sin_port = htons(4242);
	if (inet_pton(AF_INET, "10.0.2.2", &peer.sin_addr) != 1) {
		fail("inet_pton");
		close(fd);
		return 1;
	}
	ssize_t sent = sendto(fd, request, sizeof(request) - 1, 0,
		                     (struct sockaddr *)&peer, sizeof(peer));
	if (sent != (ssize_t)(sizeof(request) - 1)) {
		fail("sendto");
		close(fd);
		return 1;
	}
	printf("STARRY_UDP_TX bytes=%ld\n", (long)sent);

	pfd.fd = fd;
	pfd.events = POLLIN;
	int ready = poll(&pfd, 1, 5000);
	if (ready <= 0) {
		if (ready == 0)
			printf("STARRY_UDP_FAIL step=poll errno=0 (timeout)\n");
		else
			fail("poll");
		fflush(stdout);
		close(fd);
		return 1;
	}
	if ((pfd.revents & POLLIN) == 0) {
		errno = EIO;
		fail("poll-revents");
		close(fd);
		return 1;
	}

	ssize_t received = recvfrom(fd, response, sizeof(response) - 1, 0,
		                           (struct sockaddr *)&peer, &peer_len);
	if (received < 0) {
		fail("recvfrom");
		close(fd);
		return 1;
	}
	response[received] = '\0';
	printf("STARRY_UDP_RX bytes=%ld payload=%s\n", (long)received, response);
	if (strcmp(response, "STARRY_UDP_ACK_V1") != 0) {
		errno = EBADMSG;
		fail("payload");
		close(fd);
		return 1;
	}

	printf("STARRY_UDP_PASS\n");
	fflush(stdout);
	close(fd);
	return 0;
}
