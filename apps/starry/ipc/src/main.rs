use std::{
    ffi::CStr,
    io,
    mem::{size_of, zeroed},
    ptr,
    time::{Duration, Instant},
};

const PIPE_PAYLOAD: &[u8] = b"parent->child over pipe\0";
const PIPE_ACK: &[u8] = b"child ack over pipe\0";
const SOCKET_PAYLOAD: &[u8] = b"parent->child over socketpair\0";
const SOCKET_REPLY: &[u8] = b"child->parent over AF_UNIX socket\0";
const MSGQUEUE_PAYLOAD: &[u8] = b"parent->child over msgqueue\0";

#[repr(C)]
struct SharedPage {
    child_seen_pipe: libc::c_int,
    parent_seen_socket: libc::c_int,
    child_seen_msgqueue: libc::c_int,
    pipe_payload: [u8; 64],
    socket_payload: [u8; 64],
    msgqueue_payload: [u8; 64],
}

#[repr(C)]
struct IpcMsg {
    mtype: libc::c_long,
    text: [u8; 64],
}

struct TestState {
    pass: usize,
    fail: usize,
}

impl TestState {
    fn new() -> Self {
        Self { pass: 0, fail: 0 }
    }

    fn check(&mut self, cond: bool, name: &str) {
        if cond {
            println!("  PASS | {name}");
            self.pass += 1;
        } else {
            let err = io::Error::last_os_error();
            println!("  FAIL | {name} (errno={err})");
            self.fail += 1;
        }
    }

    fn check_ret(&mut self, ret: libc::c_long, expected: libc::c_long, name: &str) {
        if ret == expected {
            println!("  PASS | {name}");
            self.pass += 1;
        } else {
            let err = io::Error::last_os_error();
            println!("  FAIL | {name} (expected={expected} got={ret} errno={err})");
            self.fail += 1;
        }
    }
}

fn copy_cstr(dst: &mut [u8], src: &[u8]) {
    let len = src.len().min(dst.len());
    dst[..len].copy_from_slice(&src[..len]);
    if len == dst.len() {
        dst[dst.len() - 1] = 0;
    }
}

fn cstr_eq(buf: &[u8], expected: &[u8]) -> bool {
    let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let expected_nul = expected
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(expected.len());
    &buf[..nul] == &expected[..expected_nul]
}

fn print_cstr(prefix: &str, buf: &[u8]) {
    let text = CStr::from_bytes_until_nul(buf)
        .map(|s| s.to_string_lossy())
        .unwrap_or_else(|_| "<invalid-c-string>".into());
    println!("{prefix}: {text}");
}

unsafe fn read_exact_cstr(fd: libc::c_int, dst: &mut [u8]) -> bool {
    let n = libc::read(fd, dst.as_mut_ptr().cast(), dst.len() - 1);
    n > 0
}

unsafe fn write_all(fd: libc::c_int, src: &[u8]) -> bool {
    libc::write(fd, src.as_ptr().cast(), src.len()) == src.len() as libc::ssize_t
}

unsafe fn child_process(
    pipe_parent_to_child: [libc::c_int; 2],
    pipe_child_to_parent: [libc::c_int; 2],
    sockets: [libc::c_int; 2],
    msqid: libc::c_int,
    shared: *mut SharedPage,
) -> ! {
    libc::close(pipe_parent_to_child[1]);
    libc::close(pipe_child_to_parent[0]);
    libc::close(sockets[0]);

    let mut pipe_buf = [0u8; 64];
    if !read_exact_cstr(pipe_parent_to_child[0], &mut pipe_buf)
        || !cstr_eq(&pipe_buf, PIPE_PAYLOAD)
    {
        libc::_exit(10);
    }
    (*shared).child_seen_pipe = 1;
    copy_cstr(&mut (*shared).pipe_payload, &pipe_buf);
    print_cstr("IPC_CHILD_PIPE_RX", &pipe_buf);

    let mut socket_buf = [0u8; 64];
    if !read_exact_cstr(sockets[1], &mut socket_buf) || !cstr_eq(&socket_buf, SOCKET_PAYLOAD) {
        libc::_exit(11);
    }
    if !write_all(sockets[1], SOCKET_REPLY) {
        libc::_exit(12);
    }
    print_cstr("IPC_CHILD_SOCKET_RX", &socket_buf);

    let mut msg: IpcMsg = zeroed();
    let n = libc::msgrcv(
        msqid,
        (&mut msg as *mut IpcMsg).cast(),
        msg.text.len(),
        7,
        0,
    );
    if n <= 0 || !cstr_eq(&msg.text, MSGQUEUE_PAYLOAD) {
        libc::_exit(13);
    }
    (*shared).child_seen_msgqueue = 1;
    copy_cstr(&mut (*shared).msgqueue_payload, &msg.text);
    print_cstr("IPC_CHILD_MSGQUEUE_RX", &msg.text);

    if !write_all(pipe_child_to_parent[1], PIPE_ACK) {
        libc::_exit(14);
    }

    libc::close(pipe_parent_to_child[0]);
    libc::close(pipe_child_to_parent[1]);
    libc::close(sockets[1]);
    libc::_exit(0);
}

fn wait_child_ok(pid: libc::pid_t, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        let mut status = 0;
        let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if ret == pid {
            return libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
        }
        if ret < 0 {
            return false;
        }
        if start.elapsed() >= timeout {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, ptr::null_mut(), 0);
            }
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn main() {
    println!("IPC TEST START");
    println!("Channels: pipe, AF_UNIX socketpair, SysV message queue, shared mmap");

    let mut state = TestState::new();
    let mut pipe_parent_to_child = [-1; 2];
    let mut pipe_child_to_parent = [-1; 2];
    let mut sockets = [-1; 2];

    unsafe {
        state.check_ret(
            libc::pipe(pipe_parent_to_child.as_mut_ptr()).into(),
            0,
            "create parent-to-child pipe",
        );
        state.check_ret(
            libc::pipe(pipe_child_to_parent.as_mut_ptr()).into(),
            0,
            "create child-to-parent pipe",
        );
        state.check_ret(
            libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr()).into(),
            0,
            "create AF_UNIX socketpair",
        );

        let msqid = libc::msgget(libc::IPC_PRIVATE, libc::IPC_CREAT | 0o600);
        state.check(msqid >= 0, "create SysV message queue");

        let shared = libc::mmap(
            ptr::null_mut(),
            size_of::<SharedPage>(),
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
        .cast::<SharedPage>();
        state.check(shared != libc::MAP_FAILED.cast(), "create shared anonymous mmap page");

        if state.fail > 0 {
            println!("IPC TEST FAILED");
            std::process::exit(1);
        }
        ptr::write_bytes(shared, 0, 1);

        let pid = libc::fork();
        state.check(pid >= 0, "fork child process");
        if pid < 0 {
            println!("IPC TEST FAILED");
            std::process::exit(1);
        }
        if pid == 0 {
            child_process(
                pipe_parent_to_child,
                pipe_child_to_parent,
                sockets,
                msqid,
                shared,
            );
        }

        libc::close(pipe_parent_to_child[0]);
        libc::close(pipe_child_to_parent[1]);
        libc::close(sockets[1]);

        state.check(
            write_all(pipe_parent_to_child[1], PIPE_PAYLOAD),
            "send pipe payload to child",
        );
        state.check(
            write_all(sockets[0], SOCKET_PAYLOAD),
            "send socketpair payload to child",
        );

        let mut msg = IpcMsg {
            mtype: 7,
            text: [0; 64],
        };
        copy_cstr(&mut msg.text, MSGQUEUE_PAYLOAD);
        state.check_ret(
            libc::msgsnd(
                msqid,
                (&msg as *const IpcMsg).cast(),
                MSGQUEUE_PAYLOAD.len(),
                0,
            )
            .into(),
            0,
            "send SysV message to child",
        );

        let mut pipe_ack = [0u8; 64];
        state.check(
            read_exact_cstr(pipe_child_to_parent[0], &mut pipe_ack),
            "receive pipe acknowledgement from child",
        );
        state.check(
            cstr_eq(&pipe_ack, PIPE_ACK),
            "pipe acknowledgement payload matches",
        );

        let mut socket_ack = [0u8; 64];
        state.check(
            read_exact_cstr(sockets[0], &mut socket_ack),
            "receive socketpair reply from child",
        );
        state.check(
            cstr_eq(&socket_ack, SOCKET_REPLY),
            "socketpair reply payload matches",
        );

        copy_cstr(&mut (*shared).socket_payload, &socket_ack);
        (*shared).parent_seen_socket = 1;

        print_cstr("IPC_PARENT_PIPE_ACK", &pipe_ack);
        print_cstr("IPC_PARENT_SOCKET_ACK", &socket_ack);

        state.check(
            wait_child_ok(pid, Duration::from_secs(3)),
            "child exits after completing all IPC channels",
        );

        state.check(
            (*shared).child_seen_pipe == 1,
            "shared mmap records child pipe receive state",
        );
        state.check(
            cstr_eq(&(*shared).pipe_payload, PIPE_PAYLOAD),
            "shared mmap carries pipe payload observed by child",
        );
        state.check(
            (*shared).parent_seen_socket == 1,
            "shared mmap records parent socket receive state",
        );
        state.check(
            cstr_eq(&(*shared).socket_payload, SOCKET_REPLY),
            "shared mmap carries socket reply observed by parent",
        );
        state.check(
            (*shared).child_seen_msgqueue == 1,
            "shared mmap records child message-queue receive state",
        );
        state.check(
            cstr_eq(&(*shared).msgqueue_payload, MSGQUEUE_PAYLOAD),
            "shared mmap carries message-queue payload observed by child",
        );

        println!("IPC_SUMMARY: pipe=ok socketpair=ok msgqueue=ok shared_mmap=ok");
        println!("IPC TEST RESULT: {} pass, {} fail", state.pass, state.fail);

        libc::close(pipe_parent_to_child[1]);
        libc::close(pipe_child_to_parent[0]);
        libc::close(sockets[0]);
        libc::msgctl(msqid, libc::IPC_RMID, ptr::null_mut());
        libc::munmap(shared.cast(), size_of::<SharedPage>());
    }

    if state.fail > 0 {
        println!("IPC TEST FAILED");
        std::process::exit(1);
    }
    println!("IPC TEST PASSED");
}
