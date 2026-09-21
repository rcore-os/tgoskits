"""Linux CPU, network, file and package checks for the three-guest case."""
import re
import shlex

SUDO = "sudo -p 'Password: ' --"
CPU = "cat /proc/cpuinfo"
FILE_RW = (
    '(axvisor_test_dir=$(mktemp -d "$HOME/.axvisor-linux.XXXXXXXX") || exit; '
    'trap \'axvisor_test_rc=$?; rm -f "$axvisor_test_dir/probe" && '
    'rmdir "$axvisor_test_dir" || exit 1; exit "$axvisor_test_rc"\' EXIT; '
    'echo AXVISOR_FILE_RW > "$axvisor_test_dir/probe" && '
    'cat "$axvisor_test_dir/probe" && '
    'test "$(cat "$axvisor_test_dir/probe")" = AXVISOR_FILE_RW && sync)'
)
PACKAGE_STATE = (
    "command -v dpkg-query >/dev/null && "
    "(dpkg-query -W -f='${Status} ${Version}\\n' hello 2>/dev/null || echo absent)"
)
PACKAGE_CHECK = "dpkg-query -W -f='${Status}\\n' hello && LC_ALL=C hello"
VERSION = re.compile(r"install ok installed ([0-9][A-Za-z0-9.+:~\-]*)")


def valid_result(kind, result, status):
    if status != 0:
        return False
    if kind == "cpu":
        return re.search(r"(?m)^processor\s*:\s*[0-9]+\s*$", result) is not None
    if kind == "network":
        return re.search(r"5 packets transmitted, 5 (?:packets )?received, 0% packet loss", result) is not None
    if kind == "files":
        return "AXVISOR_FILE_RW" in result.splitlines()
    if kind == "package":
        return all(line in result.splitlines() for line in ("install ok installed", "Hello, world!"))
    if kind == "install":
        return re.search(r"(?m)^Setting up hello \(", result) is not None
    return True


class LinuxChecks:
    def __init__(self, prompt, emit, ping_target=None, timeout=600, proxy=None):
        self.prompt, self.say, self.timeout = prompt, emit, timeout
        self.proxy = proxy
        target = shlex.quote(ping_target) if ping_target else '"$gateway"'
        network = (
            '(for attempt in $(seq 1 30); do '
            'gateway=$(ip -4 route show default | awk \'NR==1 {print $3}\'); '
            'if [ -n "$gateway" ]; then '
            f'LC_ALL=C ping -n -c 5 -w 15 {target}; exit $?; '
            'fi; sleep 1; done; echo "no IPv4 default route" >&2; exit 1)'
        )
        self.steps = [("cpu", CPU), ("network", network),
                      ("files", FILE_RW), ("state", PACKAGE_STATE)]
        self.phase, self.buffer, self.index = "linux", "", 0
        self.done, self.passed, self.password_sent = False, False, False
        self.results = dict.fromkeys(("cpu", "network", "files", "package"), True)
        self.result = ""

    def abort(self, reason):
        if not self.done:
            self.done = True
            self.say(f"\nLINUX_VM_TEST_FAIL: {reason}. Manual input is available.")
            self.say("If interrupted during package installation, inspect apt/dpkg before exiting.")

    def query(self):
        self.password_sent = False
        kind, command = self.steps[self.index]
        if self.proxy and "apt-get " in command:
            option = shlex.quote("Acquire::http::Proxy=" + self.proxy)
            command = command.replace("apt-get ", f"apt-get -o {option} ")
            self.steps[self.index] = (kind, command)
        return command + "\n"

    def prompt_match(self, text):
        prefix = re.escape(self.prompt.rstrip())
        suffix = r"[ \t]*\Z" if self.prompt.rstrip().endswith(("$", "#")) else r"[#$][ \t]*\Z"
        return re.search(prefix + suffix, text)

    def finish_step(self, status):
        kind, _ = self.steps[self.index]
        if kind == "state":
            state = self.result.strip()
            match = VERSION.fullmatch(state)
            if status or (state != "absent" and not match):
                self.abort("cannot safely determine the original hello package state")
                return None
            package = "hello=" + match[1] if match else "hello"
            install = (f"{SUDO} env LC_ALL=C apt-get -o DPkg::Lock::Timeout=60 "
                       f"--no-remove --no-install-recommends --reinstall install -y {shlex.quote(package)}")
            cleanup = ("dpkg-query -W -f='${Status} ${Version}\\n' hello" if match else
                       f"{SUDO} env LC_ALL=C apt-get -o DPkg::Lock::Timeout=60 purge -y hello"
                       " && ! dpkg-query -W hello >/dev/null 2>&1")
            self.steps.extend([("install", install), ("package", PACKAGE_CHECK), ("cleanup", cleanup)])
        else:
            group = kind if kind in self.results else "package"
            self.results[group] &= valid_result(kind, self.result, status)
        self.index += 1
        self.phase, self.buffer = "result", ""
        if self.index < len(self.steps):
            return self.query()
        self.done, self.passed = True, all(self.results.values())
        self.say("\n========== Linux VM Test Summary ==========")
        for name, passed in self.results.items():
            self.say(f"{name}: {'PASS' if passed else 'FAIL'}")
        self.say(f"Passed: {sum(self.results.values())}/4; Failed: {4 - sum(self.results.values())}/4")
        self.say("LINUX_VM_TEST_PASS\nsuccess" if self.passed else "LINUX_VM_TEST_FAIL")
        self.say("Linux checks finished.")
        return None

    def feed(self, text):
        if self.done:
            return None
        self.buffer += text
        if self.phase == "linux":
            self.buffer = self.buffer[-8192:]
        clean = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", self.buffer).replace("\r", "")
        if len(clean) > 262144:
            self.abort("oversized shell response")
            return None
        prompt = self.prompt_match(clean)
        if self.phase == "linux" and prompt:
            self.phase, self.buffer = "result", ""
            return self.query()
        if self.phase == "result" and not self.password_sent and re.search(r"(?:^|\n)Password: $", clean):
            self.password_sent, self.buffer = True, ""
            return "orangepi\n"  # Public development password; sudo disables echo.
        if not prompt or self.phase not in ("result", "status"):
            return None
        lines = clean[:prompt.start()].strip().splitlines()
        command = "echo $?" if self.phase == "status" else self.steps[self.index][1]
        if lines and lines[0].strip() == command:
            lines.pop(0)
        if self.phase == "result":
            # Ignore empty prompts left over from console attachment.
            if not lines and command not in clean and not self.password_sent:
                return None
            self.result, self.phase, self.buffer = "\n".join(lines), "status", ""
            return "echo $?\n"
        status = int(lines[0]) if len(lines) == 1 and lines[0].isdigit() else 1
        return self.finish_step(status)
