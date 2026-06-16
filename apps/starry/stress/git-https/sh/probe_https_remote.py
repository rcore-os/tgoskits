#!/usr/bin/env python3
import http.server
import os
import shutil
import ssl
import subprocess
import sys
import threading
import time


PASS = 0
FAIL = 0
ROOT = "/tmp/git-https-test"
PORT = 9443
HOST = "127.0.0.1"
REMOTE = f"https://{HOST}:{PORT}/src.git"


def section(name):
    print()
    print(f"=== {name} ===")
    sys.stdout.flush()


def mark_pass(name):
    global PASS
    print(f"PASS: {name}")
    sys.stdout.flush()
    PASS += 1


def mark_fail(name, detail=""):
    global FAIL
    if detail:
        print(f"FAIL: {name}: {detail}")
    else:
        print(f"FAIL: {name}")
    sys.stdout.flush()
    FAIL += 1


def run_cmd(args, cwd=None, env=None, timeout=120):
    child_env = os.environ.copy()
    if env:
        child_env.update(env)
    return subprocess.run(
        args,
        cwd=cwd,
        env=child_env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )


def output_of(result):
    parts = [f"exit status {result.returncode}"]
    if result.stdout:
        parts.append("stdout:")
        parts.append(result.stdout.strip())
    if result.stderr:
        parts.append("stderr:")
        parts.append(result.stderr.strip())
    log_tail = server_log_tail()
    if log_tail:
        parts.append("server log:")
        parts.append(log_tail)
    return "\n".join(parts)


def server_log_tail():
    path = os.path.join(ROOT, "https-server.log")
    if not os.path.exists(path):
        return ""
    with open(path, encoding="utf-8", errors="replace") as f:
        data = f.read().strip()
    if len(data) > 2000:
        return data[-2000:]
    return data


class GitBackendHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        self.serve_git()

    def do_POST(self):
        self.serve_git()

    def log_message(self, fmt, *args):
        with open(os.path.join(ROOT, "https-server.log"), "a", encoding="utf-8") as log:
            log.write((fmt % args) + "\n")

    def serve_git(self):
        path, _, query = self.path.partition("?")
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length) if length else b""
        env = os.environ.copy()
        env.update(
            {
                "GIT_PROJECT_ROOT": ROOT,
                "GIT_HTTP_EXPORT_ALL": "1",
                "PATH_INFO": path,
                "QUERY_STRING": query,
                "REQUEST_METHOD": self.command,
                "CONTENT_TYPE": self.headers.get("Content-Type", ""),
                "CONTENT_LENGTH": str(length),
                "REMOTE_ADDR": self.client_address[0],
                "SERVER_PROTOCOL": self.protocol_version,
            }
        )
        proc = subprocess.run(
            ["git", "http-backend"],
            input=body,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
        with open(os.path.join(ROOT, "https-server.log"), "ab") as log:
            log.write(
                (
                    f"{self.command} {self.path} -> git http-backend rc={proc.returncode}, "
                    f"stdout={len(proc.stdout)} bytes, stderr={len(proc.stderr)} bytes\n"
                ).encode()
            )
            if proc.stderr:
                log.write(proc.stderr)
        header_blob, sep, response_body = proc.stdout.partition(b"\r\n\r\n")
        if not sep:
            header_blob, _, response_body = proc.stdout.partition(b"\n\n")

        status = 200
        headers = []
        for raw_line in header_blob.splitlines():
            if not raw_line:
                continue
            line = raw_line.decode("latin1")
            key, _, value = line.partition(":")
            if key.lower() == "status":
                status = int(value.strip().split(" ", 1)[0])
            elif key:
                headers.append((key, value.strip()))

        self.send_response(status)
        sent_length = False
        for key, value in headers:
            if key.lower() == "content-length":
                sent_length = True
            self.send_header(key, value)
        if not sent_length:
            self.send_header("Content-Length", str(len(response_body)))
        self.end_headers()
        self.wfile.write(response_body)


def start_https_git_server():
    server = http.server.ThreadingHTTPServer((HOST, PORT), GitBackendHandler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(
        certfile=os.path.join(ROOT, "cert.pem"),
        keyfile=os.path.join(ROOT, "key.pem"),
    )
    server.socket = context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


def prepare_certificate():
    result = run_cmd(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=127.0.0.1",
            "-keyout",
            os.path.join(ROOT, "key.pem"),
            "-out",
            os.path.join(ROOT, "cert.pem"),
        ],
        timeout=60,
    )
    if result.returncode == 0:
        mark_pass("generated self-signed certificate")
        return True
    else:
        mark_fail("certificate generation failed", output_of(result))
        return False


def prepare_bare_repo():
    src = os.path.join(ROOT, "src.git")
    work = os.path.join(ROOT, "work")
    os.makedirs(work, exist_ok=True)

    setup_commands = [
        (["git", "init", "--bare", src], None),
        (["git", "-C", src, "config", "http.receivepack", "true"], None),
        (["git", "init"], work),
        (["git", "config", "user.name", "T"], work),
        (["git", "config", "user.email", "t@t.com"], work),
        (["git", "checkout", "-b", "main"], work),
    ]
    for args, cwd in setup_commands:
        result = run_cmd(args, cwd=cwd)
        if result.returncode != 0:
            mark_fail("prepare source repository failed", output_of(result))
            return False

    with open(os.path.join(work, "readme.txt"), "w", encoding="utf-8") as f:
        f.write("hello from smart https\n")

    work_commands = [
        ["git", "add", "readme.txt"],
        ["git", "commit", "-m", "init"],
        ["git", "remote", "add", "origin", src],
        ["git", "push", "-u", "origin", "main"],
    ]
    for args in work_commands:
        result = run_cmd(args, cwd=work)
        if result.returncode != 0:
            mark_fail("prepare source repository failed", output_of(result))
            return False

    result = run_cmd(["git", "-C", src, "symbolic-ref", "HEAD", "refs/heads/main"])
    if result.returncode != 0:
        mark_fail("prepare source repository failed", output_of(result))
        return False

    result = run_cmd(["git", "--git-dir", src, "show", "main:readme.txt"])
    if result.returncode != 0 or "hello from smart https" not in result.stdout:
        mark_fail("prepared bare source repository is missing main:readme.txt", output_of(result))
        return False

    mark_pass("prepared bare source repository")
    return True


def git_env():
    return {
        "GIT_SSL_NO_VERIFY": "true",
        "GIT_TERMINAL_PROMPT": "0",
        "NO_PROXY": "127.0.0.1,localhost",
        "no_proxy": "127.0.0.1,localhost",
        "ALL_PROXY": "",
        "HTTPS_PROXY": "",
        "HTTP_PROXY": "",
        "all_proxy": "",
        "https_proxy": "",
        "http_proxy": "",
    }


def test_ls_remote():
    result = run_cmd(["git", "ls-remote", REMOTE, "refs/heads/main"], env=git_env())
    if result.returncode == 0 and "refs/heads/main" in result.stdout:
        mark_pass("git ls-remote over HTTPS")
    else:
        mark_fail("git ls-remote over HTTPS failed", output_of(result))


def test_clone():
    clone_dir = os.path.join(ROOT, "clone-https")
    result = run_cmd(["git", "clone", REMOTE, clone_dir], env=git_env())
    readme = os.path.join(clone_dir, "readme.txt")
    if result.returncode == 0 and os.path.isdir(os.path.join(clone_dir, ".git")) and os.path.isfile(readme):
        with open(readme, encoding="utf-8") as f:
            content = f.read()
        if "hello from smart https" in content:
            mark_pass("git clone HTTPS remote URL")
            return
    mark_fail("git clone HTTPS remote URL failed", output_of(result))


def test_closed_port_clone():
    result = run_cmd(
        ["git", "clone", f"https://{HOST}:19443/src.git", os.path.join(ROOT, "closed-port")],
        env=git_env(),
        timeout=30,
    )
    if result.returncode == 0:
        mark_fail("git clone from closed HTTPS port unexpectedly succeeded")
    else:
        mark_pass("git clone from closed HTTPS port fails")


def commit_and_push(work, message, line):
    with open(os.path.join(work, "readme.txt"), "a", encoding="utf-8") as f:
        f.write(line + "\n")
    for args in [
        ["git", "add", "readme.txt"],
        ["git", "commit", "-m", message],
        ["git", "push", "origin", "main"],
    ]:
        result = run_cmd(args, cwd=work)
        if result.returncode != 0:
            return result
    return result


def test_fetch():
    work = os.path.join(ROOT, "work")
    result = commit_and_push(work, "second", "second commit")
    if result.returncode != 0:
        mark_fail("prepare second commit failed", output_of(result))
        return

    clone_dir = os.path.join(ROOT, "clone-https")
    result = run_cmd(["git", "-C", clone_dir, "fetch", "origin", "main"], env=git_env())
    if result.returncode != 0:
        mark_fail("git fetch HTTPS remote URL failed", output_of(result))
        return

    result = run_cmd(["git", "-C", clone_dir, "show", "--quiet", "--format=%s", "FETCH_HEAD"])
    if result.returncode == 0 and "second" in result.stdout:
        mark_pass("git fetch HTTPS remote URL")
    else:
        mark_fail("git fetch HTTPS remote URL failed", output_of(result))


def test_pull():
    pull_client = os.path.join(ROOT, "pull-client")
    result = run_cmd(["git", "clone", REMOTE, pull_client], env=git_env())
    if result.returncode != 0:
        mark_fail("prepare pull client failed", output_of(result))
        return

    result = commit_and_push(os.path.join(ROOT, "work"), "third", "third commit")
    if result.returncode != 0:
        mark_fail("prepare third commit failed", output_of(result))
        return

    result = run_cmd(
        ["git", "-C", pull_client, "pull", "--ff-only", "origin", "main"],
        env=git_env(),
    )
    readme = os.path.join(pull_client, "readme.txt")
    if result.returncode == 0:
        with open(readme, encoding="utf-8") as f:
            content = f.read()
        if "third commit" in content:
            mark_pass("git pull HTTPS remote URL")
            return
    mark_fail("git pull HTTPS remote URL failed", output_of(result))


def test_push():
    push_client = os.path.join(ROOT, "push-client")
    result = run_cmd(["git", "clone", REMOTE, push_client], env=git_env())
    if result.returncode != 0:
        mark_fail("prepare push client failed", output_of(result))
        return

    for args in [
        ["git", "config", "user.name", "T"],
        ["git", "config", "user.email", "t@t.com"],
    ]:
        result = run_cmd(args, cwd=push_client)
        if result.returncode != 0:
            mark_fail("prepare push client failed", output_of(result))
            return

    with open(os.path.join(push_client, "pushed.txt"), "w", encoding="utf-8") as f:
        f.write("client https push\n")

    for args in [
        ["git", "add", "pushed.txt"],
        ["git", "commit", "-m", "client-https-push"],
        ["git", "push", "origin", "main"],
    ]:
        result = run_cmd(args, cwd=push_client, env=git_env())
        if result.returncode != 0:
            mark_fail("git push HTTPS remote URL failed", output_of(result))
            return

    result = run_cmd(["git", "--git-dir", os.path.join(ROOT, "src.git"), "show", "main:pushed.txt"])
    if result.returncode == 0 and "client https push" in result.stdout:
        mark_pass("git push HTTPS remote URL")
    else:
        mark_fail("git push HTTPS remote URL failed", output_of(result))


def main():
    shutil.rmtree(ROOT, ignore_errors=True)
    os.makedirs(ROOT, exist_ok=True)

    section("prep certificate")
    prepared = prepare_certificate()

    section("prep bare repo")
    prepared = prepare_bare_repo() and prepared

    section("start HTTPS smart Git server")
    server = None
    if prepared:
        try:
            server = start_https_git_server()
            time.sleep(1)
            mark_pass("HTTPS smart Git server started")
        except Exception as exc:
            mark_fail("HTTPS smart Git server failed to start", str(exc))
    else:
        mark_fail("HTTPS smart Git server skipped after setup failure")

    if server is None:
        print()
        print(f"RESULT: PASS={PASS} FAIL={FAIL}")
        print("GIT_HTTPS_HAS_FAILURES")
        return 1

    section("1. git ls-remote https://")
    test_ls_remote()

    section("2. git clone https://")
    test_clone()

    section("3. clone from closed HTTPS port should fail")
    test_closed_port_clone()

    section("4. git fetch from https://")
    test_fetch()

    section("5. git pull from https://")
    test_pull()

    section("6. git push to https://")
    test_push()

    if server is not None:
        server.shutdown()

    print()
    print(f"RESULT: PASS={PASS} FAIL={FAIL}")
    if FAIL == 0:
        print("GIT_HTTPS_CLONE_ALL_PASSED")
        print("GIT_HTTPS_FETCH_ALL_PASSED")
        print("GIT_HTTPS_PULL_ALL_PASSED")
        print("GIT_HTTPS_PUSH_ALL_PASSED")
        print("GIT_HTTPS_ALL_PASSED")
        return 0

    print("GIT_HTTPS_HAS_FAILURES")
    return 1


if __name__ == "__main__":
    sys.exit(main())
