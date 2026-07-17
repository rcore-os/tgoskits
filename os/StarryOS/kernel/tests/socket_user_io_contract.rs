//! Socket syscalls must not fault user memory while an ax-net socket lock is held.

const SOCKET_FILE: &str = include_str!("../src/file/net.rs");
const SOCKET_SYSCALL: &str = include_str!("../src/syscall/net/io.rs");

#[test]
fn socket_file_and_syscall_io_share_the_user_staging_boundary() {
    let file_read = function_body(SOCKET_FILE, "fn read(&self, dst: &mut IoDst)");
    let file_write = function_body(SOCKET_FILE, "fn write(&self, src: &mut IoSrc)");
    let send_syscall = function_body(SOCKET_SYSCALL, "fn send_impl(");
    let recv_syscall = function_body(SOCKET_SYSCALL, "fn recv_impl(");

    assert!(file_read.contains("recv_to_user("));
    assert!(file_write.contains("send_from_user("));
    assert!(send_syscall.contains("socket.send_from_user("));
    assert!(recv_syscall.contains("socket.recv_to_user("));
    assert!(!send_syscall.contains("socket.send("));
    assert!(!recv_syscall.contains("socket.recv("));
}

#[test]
fn user_copy_happens_outside_the_protocol_dispatch() {
    let send = function_body(SOCKET_FILE, "pub(crate) fn send_from_user(");
    let recv = function_body(SOCKET_FILE, "pub(crate) fn recv_to_user(");

    assert_ordered(send, "src.read(", "self.inner.send(");
    assert_ordered(recv, "self.inner.recv(", "dst.write(");
    assert!(recv.contains("SOCKET_USER_IO_CHUNK"));
}

#[test]
fn message_oriented_send_is_bounded_without_fragmenting_a_message() {
    let send = function_body(SOCKET_FILE, "pub(crate) fn send_from_user(");
    let stage_len = function_body(SOCKET_FILE, "fn send_stage_len(");

    assert!(send.contains("send_stage_len("));
    assert!(stage_len.contains("SOCK_STREAM"));
    assert!(stage_len.contains("SOCKET_USER_IO_CHUNK"));
    assert!(stage_len.contains("AxError::MessageTooLong"));
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    let body = &source[start..];
    let body_start = body
        .find('{')
        .unwrap_or_else(|| panic!("missing body for `{signature}`"));
    let body = &body[body_start + 1..];
    let mut depth = 1usize;
    for (offset, byte) in body.bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &body[..offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function `{signature}`");
}

fn assert_ordered(source: &str, first: &str, second: &str) {
    let first = source
        .find(first)
        .unwrap_or_else(|| panic!("missing `{first}`"));
    let second = source
        .find(second)
        .unwrap_or_else(|| panic!("missing `{second}`"));
    assert!(first < second, "`{first}` must precede `{second}`");
}
