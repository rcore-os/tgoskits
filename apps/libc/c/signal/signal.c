#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

void test_term() {
  if (fork() == 0) {
    kill(getpid(), SIGTERM);
    while (1)
      ;
  }
  wait(NULL);
  puts("test_term ok");
}

static void signal_handler(int signum) {
  static int count = 0;
  count++;
  printf("Received signal %d, count=%d\n", signum, count);
  if (count > 1) {
    return;
  }
  // This should be blocked and won't cause recursion
  kill(getpid(), SIGTERM);
  printf("End, count=%d\n", count);
}

void test_sigaction() {
  struct sigaction sa = {0};
  sa.sa_handler = signal_handler;
  sigaction(SIGTERM, &sa, NULL);
  kill(getpid(), SIGTERM);
  puts("test_sigaction ok1");

  sa.sa_handler = (void (*)(int))1;
  sigaction(SIGTERM, &sa, NULL);
  kill(getpid(), SIGTERM);
  puts("test_sigaction ok2");

  sa.sa_handler = (void (*)(int))0;
  sigaction(SIGTERM, &sa, NULL);
}

void test_sigprocmask() {
  sigset_t set, set2;
  sigemptyset(&set);
  sigaddset(&set, SIGTERM);
  sigprocmask(SIG_BLOCK, &set, NULL);
  kill(getpid(), SIGTERM);

  sigpending(&set2);
  if (sigismember(&set2, SIGTERM)) {
    puts("test_sigprocmask ok1");
  }

  // Ignore SIGTERM for once
  struct sigaction sa = {0};
  sa.sa_handler = (void (*)(int))1;
  sigaction(SIGTERM, &sa, NULL);

  sigdelset(&set, SIGTERM);
  sigprocmask(SIG_SETMASK, &set, NULL);

  sigpending(&set2);
  if (!sigismember(&set2, SIGTERM)) {
    puts("test_sigprocmask ok2");
  }

  sa.sa_handler = (void (*)(int))0;
  sigaction(SIGTERM, &sa, NULL);
}

void test_sigkill_stop() {
  struct sigaction sa = {0};
  sa.sa_handler = signal_handler;
  if (sigaction(SIGKILL, &sa, NULL) < 0) {
    puts("test_sigkill_stop ok1");
  }
  if (sigaction(SIGSTOP, &sa, NULL) < 0) {
    puts("test_sigkill_stop ok2");
  }
}

void test_sigwait() {
  int pid = fork();
  if (pid == 0) {
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGTERM);
    sigprocmask(SIG_BLOCK, &set, NULL);
    int sig;
    sigwait(&set, &sig);
    if (sig == SIGTERM) {
      puts("test_sigwait ok1");
    }
    exit(0);
  }
  sleep(1);
  kill(pid, SIGTERM);
  wait(NULL);
  puts("test_sigwait ok2");

  sigset_t set;
  sigemptyset(&set);
  sigaddset(&set, SIGTERM);
  sigprocmask(SIG_BLOCK, &set, NULL);
  struct timespec ts = {1, 0};
  if (sigtimedwait(&set, NULL, &ts) < 0 && errno == EAGAIN) {
    puts("test_sigwait ok3");
  }
  sigprocmask(SIG_UNBLOCK, &set, NULL);
}

static void signal_handler2(int signum) { puts("test_sigsuspend ok1"); }
static void signal_handler3(int signum) { puts("test_sigsuspend ok3"); }
void test_sigsuspend() {
  int pid = fork();
  if (pid == 0) {
    struct sigaction sa = {0};
    sa.sa_handler = signal_handler2;
    sigaction(SIGUSR1, &sa, NULL);

    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGTERM);
    sigsuspend(&set);
    // SIGTERM is handled immediately after so this won't run
    // To ensure it, we check this return code below
    exit(0);
  }
  sleep(1);
  kill(pid, SIGTERM);
  sleep(1);
  kill(pid, SIGUSR1);
  int status;
  wait(&status);
  if (status != 0) {
    puts("test_sigsuspend ok2");
  }

  pid = fork();
  if (pid == 0) {
    // Ignore SIGTERM
    struct sigaction sa = {0};
    sa.sa_handler = (void (*)(int))1;
    sigaction(SIGTERM, &sa, NULL);

    sa.sa_handler = signal_handler3;
    sigaction(SIGUSR1, &sa, NULL);

    sigset_t set;
    sigemptyset(&set);
    sigsuspend(&set);
    exit(0);
  }
  sleep(1);
  kill(pid, SIGTERM); // SIGTERM is ignored so sigsuspend won't unblock
  sleep(1);
  kill(pid, SIGUSR1);
}

int main() {
  test_term();
  test_sigaction();
  test_sigprocmask();
  test_sigkill_stop();
  test_sigwait();
  test_sigsuspend();
  return 0;
}