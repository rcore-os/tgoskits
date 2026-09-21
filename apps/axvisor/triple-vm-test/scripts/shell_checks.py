"""Native ArceOS and Zephyr shell checks for the three-guest case."""
import re


def arceos_result(index, result):
    if re.search(r"unknown command|command not found|error:|fatal|panic", result, re.I):
        return False
    if index == 0:
        return "Available commands:" in result and all(
            re.search(rf"(?m)^\s+{name}\s*$", result) for name in ("help", "uname", "exit"))
    return re.search(r"(?m)^ArceOS [0-9]+\.[0-9]+\.[0-9]+ aarch64(?:[ \t].*)?$", result) is not None


def zephyr_result(index, result):
    if re.search(r"unknown command|command not found|error:|fatal", result, re.I):
        return False
    if index == 0:
        return re.search(r"(?m)^Zephyr version [0-9]+\.[0-9]+\.[0-9]+[^\r\n]*$", result) is not None
    if index == 1:
        return "Threads:" in result and all(re.search(
            rf"(?m)^\s*\*?0x[0-9a-fA-F]+\s+{name}\s*$", result) for name in ("shell_uart", "idle"))
    return all(re.search(rf"(?m)^- {re.escape(device)} \(READY\)\s*$", result)
               for device in ("serial@feb50000", "interrupt-controller@fe600000"))


class ShellChecks:
    def __init__(self, name, prompt, queries, validate, emit, timeout=30):
        self.prompt, self.say, self.timeout = prompt, emit, timeout
        self.name, self.queries, self.validate = name, queries, validate
        self.phase, self.buffer, self.index = "guest", "", 0
        self.results, self.done, self.passed = [], False, False

    def abort(self, reason):
        if not self.done:
            self.done = True
            self.say(f"\n{self.name.upper()}_VM_TEST_FAIL: {reason}. Manual input is available.")

    def query(self):
        return self.queries[self.index] + "\n"

    def feed(self, text):
        if self.done:
            return None
        self.buffer += text
        if self.phase != "result":
            self.buffer = self.buffer[-8192:]
        clean = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", self.buffer).replace("\r", "")
        prompt = re.search(re.escape(self.prompt.rstrip()) + r"[ \t]*", clean)
        if self.phase == "guest" and prompt:
            self.phase, self.buffer = "result", ""
            return self.query()
        if self.phase != "result":
            return None
        if len(clean) > 65536:
            self.abort("oversized shell response")
            return None
        # A prompt left over from console attachment is not this command's
        # response. Wait for its real echo before looking for completion.
        command = re.escape(self.queries[self.index])
        echo = re.search(rf"(?m)(?:^|{re.escape(self.prompt.rstrip())})[ \t]*{command}\n", clean)
        if not echo:
            return None
        response = clean[echo.end():]
        prompt = re.search(re.escape(self.prompt.rstrip()) + r"[ \t]*", response)
        if not prompt:
            return None
        result = response[:prompt.start()].strip()
        ok = bool(result) and self.validate(self.index, result)
        self.results.append(ok)
        self.index += 1
        self.buffer = ""
        if self.index < len(self.queries):
            return self.query()
        self.done, self.passed = True, all(self.results)
        self.say(f"\n========== {self.name} Test Summary ==========\n"
                 f"Passed: {sum(self.results)}/{len(self.queries)}; "
                 f"Failed: {len(self.queries) - sum(self.results)}/{len(self.queries)}")
        self.say(f"{self.name.upper()}_VM_TEST_{'PASS' if self.passed else 'FAIL'}")
        return None
