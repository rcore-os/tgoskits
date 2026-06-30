/*
 * tcp-defer-accept-probe.c
 *
 * Decisive experiment for the question raised by the Apache smoke review:
 *   "Is the readiness curl timeout caused by setsockopt(TCP_DEFER_ACCEPT)
 *    returning ENOPROTOOPT (errno 92), or is AH00076 only a warning while the
 *    listen socket stays usable?"
 *
 * This reproduces Apache/APR make_sock() ordering and, crucially, its error
 * handling: APR logs AH00076 as a WARNING and then CONTINUES. So this probe
 * does NOT abort when setsockopt fails. After that, it performs a real local
 * connect()/accept() on the same listen socket, exactly like the readiness
 * curl to 127.0.0.1:8080 does.
 *
 * Interpretation:
 *   - kernel WITHOUT fix: setsockopt returns -1/errno 92 (ENOPROTOOPT).
 *       * if connect/accept STILL succeeds -> AH00076 is only a warning;
 *         errno 92 is NOT the cause of the curl timeout.
 *       * if connect/accept FAILS          -> errno 92 broke the listen
 *         socket; it IS the root cause.
 *   - kernel WITH fix: setsockopt returns 0; connect/accept succeeds.
 */
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <signal.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>

#define PORT 18080

static void on_alarm(int sig) {
    (void)sig;
    /* Just interrupt the blocking accept(). */
}

int main(void) {
    int setsockopt_failed = 0;

    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        printf("PROBE_FAIL socket errno=%d (%s)\n", errno, strerror(errno));
        return 2;
    }

    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = inet_addr("127.0.0.1");
    addr.sin_port = htons(PORT);

    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        printf("PROBE_FAIL bind errno=%d (%s)\n", errno, strerror(errno));
        close(fd);
        return 2;
    }
    if (listen(fd, 8) < 0) {
        printf("PROBE_FAIL listen errno=%d (%s)\n", errno, strerror(errno));
        close(fd);
        return 2;
    }

    /* APR_TCP_DEFER_ACCEPT path. Do NOT abort on failure: this mirrors
     * Apache logging AH00076 as a warning and continuing. */
    int secs = 30;
    int rc = setsockopt(fd, IPPROTO_TCP, TCP_DEFER_ACCEPT, &secs, sizeof(secs));
    int saved = errno;
    if (rc == 0) {
        printf("TCP_DEFER_ACCEPT_SET_OK rc=0\n");
    } else {
        setsockopt_failed = 1;
        printf("TCP_DEFER_ACCEPT_SET_FAIL rc=%d errno=%d (%s) "
               "(continuing, like Apache AH00076 warning)\n",
               rc, saved, strerror(saved));
    }

    /* Now the decisive part: can a client still connect and be accepted? */
    pid_t pid = fork();
    if (pid < 0) {
        printf("PROBE_FAIL fork errno=%d (%s)\n", errno, strerror(errno));
        close(fd);
        return 2;
    }

    if (pid == 0) {
        /* Child: the "curl" client. */
        close(fd);
        int c = socket(AF_INET, SOCK_STREAM, 0);
        if (c < 0) {
            _exit(11);
        }
        struct sockaddr_in caddr;
        memset(&caddr, 0, sizeof(caddr));
        caddr.sin_family = AF_INET;
        caddr.sin_addr.s_addr = inet_addr("127.0.0.1");
        caddr.sin_port = htons(PORT);
        if (connect(c, (struct sockaddr *)&caddr, sizeof(caddr)) < 0) {
            printf("CLIENT_CONNECT_FAIL errno=%d (%s)\n", errno, strerror(errno));
            close(c);
            _exit(12);
        }
        printf("CLIENT_CONNECT_OK\n");
        write(c, "PING", 4);
        close(c);
        _exit(0);
    }

    /* Parent: the Apache listener. accept() with a 5s alarm timeout. */
    signal(SIGALRM, on_alarm);
    alarm(5);

    struct sockaddr_in peer;
    socklen_t plen = sizeof(peer);
    int conn = accept(fd, (struct sockaddr *)&peer, &plen);
    int accept_errno = errno;
    alarm(0);

    int connect_works;
    if (conn >= 0) {
        char buf[16];
        ssize_t n = read(conn, buf, sizeof(buf) - 1);
        if (n > 0) {
            buf[n] = '\0';
            printf("SERVER_ACCEPT_OK got=%zd bytes payload=\"%s\"\n", n, buf);
        } else {
            printf("SERVER_ACCEPT_OK got=%zd bytes (no payload)\n", n);
        }
        close(conn);
        connect_works = 1;
    } else {
        printf("SERVER_ACCEPT_FAIL errno=%d (%s)\n",
               accept_errno, strerror(accept_errno));
        connect_works = 0;
    }

    int status = 0;
    waitpid(pid, &status, 0);
    close(fd);

    /* Verdict. */
    if (setsockopt_failed && connect_works) {
        printf("VERDICT: setsockopt(TCP_DEFER_ACCEPT) FAILED but "
               "connect/accept STILL WORKS -> AH00076 is only a warning; "
               "errno 92 is NOT the curl-timeout root cause\n");
        printf("PROBE_RESULT_WARNING_ONLY\n");
        return 0;
    }
    if (setsockopt_failed && !connect_works) {
        printf("VERDICT: setsockopt(TCP_DEFER_ACCEPT) FAILED and "
               "connect/accept ALSO FAILS -> errno 92 broke the listen "
               "socket; it IS the curl-timeout root cause\n");
        printf("PROBE_RESULT_ROOT_CAUSE\n");
        return 1;
    }
    if (!setsockopt_failed && connect_works) {
        printf("VERDICT: setsockopt(TCP_DEFER_ACCEPT) OK and connect/accept "
               "works -> fixed kernel behaves correctly\n");
        printf("PROBE_RESULT_FIXED_OK\n");
        return 0;
    }
    printf("VERDICT: setsockopt OK but connect/accept FAILED -> unexpected, "
           "unrelated socket problem\n");
    printf("PROBE_RESULT_UNEXPECTED\n");
    return 1;
}
