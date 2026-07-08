#define _GNU_SOURCE

#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef NT_PRSTATUS
#define NT_PRSTATUS 1
#endif
#ifndef PTRACE_GETREGSET
#define PTRACE_GETREGSET 0x4204
#endif
#ifndef PTRACE_SETREGSET
#define PTRACE_SETREGSET 0x4205
#endif

#define TRACE_WORD_INITIAL 0x12345678UL
#define TRACE_WORD_PATCHED 0x55aa7711UL
#define RISCV_EBREAK_INSN 0x00100073UL

struct trace_addrs {
    uintptr_t data_addr;
    uintptr_t text_addr;
};

static volatile int trace_return_value = 7;
static volatile sig_atomic_t sigchld_seen = 0;

__attribute__((noinline, aligned(8))) static int trace_target_function(void)
{
    return trace_return_value;
}

static void sigchld_handler(int signo)
{
    (void)signo;
    sigchld_seen = 1;
}

static int traceme_stop_child(void *arg)
{
    (void)arg;
    if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
        _exit(101);
    }
    if (kill(getpid(), SIGSTOP) != 0) {
        _exit(102);
    }
    _exit(0);
}

struct riscv_user_regs {
    unsigned long pc;
    unsigned long ra;
    unsigned long sp;
    unsigned long gp;
    unsigned long tp;
    unsigned long t0;
    unsigned long t1;
    unsigned long t2;
    unsigned long s0;
    unsigned long s1;
    unsigned long a0;
    unsigned long a1;
    unsigned long a2;
    unsigned long a3;
    unsigned long a4;
    unsigned long a5;
    unsigned long a6;
    unsigned long a7;
    unsigned long s2;
    unsigned long s3;
    unsigned long s4;
    unsigned long s5;
    unsigned long s6;
    unsigned long s7;
    unsigned long s8;
    unsigned long s9;
    unsigned long s10;
    unsigned long s11;
    unsigned long t3;
    unsigned long t4;
    unsigned long t5;
    unsigned long t6;
};

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static int remote_read_word(pid_t pid, uintptr_t addr, unsigned long *out)
{
    struct iovec local = {.iov_base = out, .iov_len = sizeof(*out)};
    struct iovec remote = {.iov_base = (void *)addr, .iov_len = sizeof(*out)};
    ssize_t bytes = process_vm_readv(pid, &local, 1, &remote, 1, 0);
    if (bytes != (ssize_t)sizeof(*out)) {
        errno = bytes < 0 ? errno : EIO;
        return -1;
    }
    return 0;
}

static int remote_write_word(pid_t pid, uintptr_t addr, unsigned long word)
{
    struct iovec local = {.iov_base = &word, .iov_len = sizeof(word)};
    struct iovec remote = {.iov_base = (void *)addr, .iov_len = sizeof(word)};
    ssize_t bytes = process_vm_writev(pid, &local, 1, &remote, 1, 0);
    if (bytes != (ssize_t)sizeof(word)) {
        errno = bytes < 0 ? errno : EIO;
        return -1;
    }
    return 0;
}

static int proc_mem_read_word(int fd, uintptr_t addr, unsigned long *out)
{
    ssize_t bytes = pread(fd, out, sizeof(*out), (off_t)addr);
    if (bytes != (ssize_t)sizeof(*out)) {
        errno = bytes < 0 ? errno : EIO;
        return -1;
    }
    return 0;
}

static int proc_mem_write_word(int fd, uintptr_t addr, unsigned long word)
{
    ssize_t bytes = pwrite(fd, &word, sizeof(word), (off_t)addr);
    if (bytes != (ssize_t)sizeof(word)) {
        errno = bytes < 0 ? errno : EIO;
        return -1;
    }
    return 0;
}

int main(void)
{
    struct sigaction chld_action;
    memset(&chld_action, 0, sizeof(chld_action));
    chld_action.sa_handler = sigchld_handler;
    sigemptyset(&chld_action.sa_mask);
    chld_action.sa_flags = SA_RESTART;
    if (sigaction(SIGCHLD, &chld_action, NULL) != 0) {
        return fail("sigaction SIGCHLD");
    }

    pid_t check_pid = fork();
    if (check_pid < 0) {
        return fail("initial fork");
    }

    if (check_pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(101);
        }
        if (kill(getpid(), SIGSTOP) != 0) {
            _exit(102);
        }
        _exit(0);
    }

    int check_status = 0;
    if (waitpid(check_pid, &check_status, 0) != check_pid || !WIFSTOPPED(check_status)
        || WSTOPSIG(check_status) != SIGSTOP) {
        printf("FAIL: expected linux_check_ptrace_features-style SIGSTOP, status=%#x\n",
               check_status);
        return 1;
    }
    if (kill(check_pid, SIGKILL) != 0) {
        return fail("kill initial child");
    }
    if (waitpid(check_pid, &check_status, 0) != check_pid || !WIFSIGNALED(check_status)
        || WTERMSIG(check_status) != SIGKILL) {
        printf("FAIL: expected initial child SIGKILL, status=%#x\n", check_status);
        return 1;
    }

    enum { STACK_SIZE = 4096 * 4 };
    char *clone_stack = malloc(STACK_SIZE);
    if (clone_stack == NULL) {
        return fail("malloc clone stack");
    }
    pid_t clone_pid = clone(traceme_stop_child, clone_stack + STACK_SIZE, CLONE_VM | SIGCHLD, NULL);
    if (clone_pid < 0) {
        free(clone_stack);
        return fail("clone");
    }
    int clone_status = 0;
    if (waitpid(clone_pid, &clone_status, 0) != clone_pid || !WIFSTOPPED(clone_status)
        || WSTOPSIG(clone_status) != SIGSTOP) {
        printf("FAIL: expected CLONE_VM traced child SIGSTOP, status=%#x\n", clone_status);
        free(clone_stack);
        return 1;
    }
    if (kill(clone_pid, SIGKILL) != 0) {
        free(clone_stack);
        return fail("kill clone child");
    }
    if (waitpid(clone_pid, &clone_status, 0) != clone_pid || !WIFSIGNALED(clone_status)
        || WTERMSIG(clone_status) != SIGKILL) {
        printf("FAIL: expected CLONE_VM traced child SIGKILL, status=%#x\n", clone_status);
        free(clone_stack);
        return 1;
    }
    free(clone_stack);

    int pipefd[2];
    if (pipe(pipefd) != 0) {
        return fail("pipe");
    }

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        close(pipefd[0]);
        volatile unsigned long *trace_word = malloc(sizeof(*trace_word));
        if (trace_word == NULL) {
            _exit(100);
        }
        *trace_word = TRACE_WORD_INITIAL;
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(101);
        }
        struct trace_addrs addrs = {
            .data_addr = (uintptr_t)trace_word,
            .text_addr = (uintptr_t)trace_target_function,
        };
        if (write(pipefd[1], &addrs, sizeof(addrs)) != (ssize_t)sizeof(addrs)) {
            _exit(102);
        }
        close(pipefd[1]);
        if (kill(getpid(), SIGSTOP) != 0) {
            _exit(103);
        }
        if (*trace_word != TRACE_WORD_PATCHED) {
            _exit(104);
        }
        if (trace_target_function() != 7) {
            _exit(105);
        }
        _exit(42);
    }

    close(pipefd[1]);
    struct trace_addrs addrs = {0};
    if (read(pipefd[0], &addrs, sizeof(addrs)) != (ssize_t)sizeof(addrs)) {
        return fail("read trace addrs");
    }
    close(pipefd[0]);

    int status = 0;
    if (waitpid(pid, &status, 0) != pid || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: expected initial SIGSTOP, status=%#x\n", status);
        return 1;
    }

    struct riscv_user_regs regs = {0};
    struct iovec iov = {.iov_base = &regs, .iov_len = sizeof(regs)};
    if (ptrace(PTRACE_GETREGSET, pid, (void *)NT_PRSTATUS, &iov) != 0) {
        return fail("ptrace getregset");
    }
    if (regs.pc == 0 || regs.sp == 0 || (size_t)iov.iov_len != sizeof(regs)) {
        printf("FAIL: invalid initial registers pc=%#lx sp=%#lx len=%zu\n",
               regs.pc, regs.sp, (size_t)iov.iov_len);
        return 1;
    }

    errno = 0;
    long word = ptrace(PTRACE_PEEKDATA, pid, (void *)addrs.data_addr, NULL);
    if ((word == -1 && errno != 0) || (unsigned long)word != TRACE_WORD_INITIAL) {
        return fail("ptrace peekdata");
    }
    if (ptrace(PTRACE_POKEDATA, pid, (void *)addrs.data_addr, (void *)TRACE_WORD_PATCHED) != 0) {
        return fail("ptrace pokedata");
    }

    errno = 0;
    long text_word = ptrace(PTRACE_PEEKDATA, pid, (void *)addrs.text_addr, NULL);
    if (text_word == -1 && errno != 0) {
        return fail("ptrace peek text");
    }
    unsigned long breakpoint_word =
        (((unsigned long)text_word) & ~0xffffffffUL) | RISCV_EBREAK_INSN;
    if (ptrace(PTRACE_POKEDATA, pid, (void *)addrs.text_addr, (void *)breakpoint_word) != 0) {
        return fail("ptrace poke breakpoint");
    }
    if (ptrace(PTRACE_POKEDATA, pid, (void *)addrs.text_addr, (void *)text_word) != 0) {
        return fail("ptrace restore text before process_vm");
    }

    unsigned long vm_text_word = 0;
    if (remote_read_word(pid, addrs.text_addr, &vm_text_word) != 0 ||
        vm_text_word != (unsigned long)text_word) {
        return fail("process_vm_readv text");
    }
    if (remote_write_word(pid, addrs.text_addr, breakpoint_word) != 0) {
        return fail("process_vm_writev breakpoint");
    }
    if (remote_write_word(pid, addrs.text_addr, (unsigned long)text_word) != 0) {
        return fail("process_vm_writev restore before proc mem");
    }

    char mem_path[64];
    snprintf(mem_path, sizeof(mem_path), "/proc/%d/mem", pid);
    int mem_fd = open(mem_path, O_RDWR);
    if (mem_fd < 0) {
        return fail("open proc mem");
    }
    unsigned long proc_mem_text_word = 0;
    if (proc_mem_read_word(mem_fd, addrs.text_addr, &proc_mem_text_word) != 0 ||
        proc_mem_text_word != (unsigned long)text_word) {
        close(mem_fd);
        return fail("proc mem pread text");
    }
    if (proc_mem_write_word(mem_fd, addrs.text_addr, breakpoint_word) != 0) {
        close(mem_fd);
        return fail("proc mem pwrite breakpoint");
    }

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        close(mem_fd);
        return fail("ptrace cont to breakpoint");
    }
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: expected breakpoint SIGTRAP, status=%#x\n", status);
        close(mem_fd);
        return 1;
    }

    memset(&regs, 0, sizeof(regs));
    iov.iov_len = sizeof(regs);
    if (ptrace(PTRACE_GETREGSET, pid, (void *)NT_PRSTATUS, &iov) != 0) {
        close(mem_fd);
        return fail("ptrace getregset at breakpoint");
    }
    if (regs.pc != addrs.text_addr) {
        printf("FAIL: expected breakpoint pc=%#lx, got %#lx\n",
               (unsigned long)addrs.text_addr, regs.pc);
        close(mem_fd);
        return 1;
    }
    if (proc_mem_write_word(mem_fd, addrs.text_addr, (unsigned long)text_word) != 0) {
        close(mem_fd);
        return fail("proc mem pwrite restore text");
    }
    close(mem_fd);
    regs.pc = addrs.text_addr;
    iov.iov_len = sizeof(regs);
    if (ptrace(PTRACE_SETREGSET, pid, (void *)NT_PRSTATUS, &iov) != 0) {
        return fail("ptrace reset breakpoint pc");
    }

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("ptrace cont after breakpoint");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status) || WEXITSTATUS(status) != 42) {
        printf("FAIL: expected child exit 42, status=%#x\n", status);
        return 1;
    }

    puts("DONE: 1 pass, 0 fail");
    return 0;
}
