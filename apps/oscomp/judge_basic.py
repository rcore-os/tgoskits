import json
import sys
from typing import List
import re


class TestBase:
    class AssertFail(RuntimeError):
        pass

    def __init__(self, name, count):
        self.name = "test_" + name
        self.count = count
        self.result = []

    def test(self, data):
        pass

    def assert_util(self, func, rep, msg, *args):
        self.result.append({
            "rep": rep,
            "res": func(*args),
            "arg": args,
            "msg": msg
        })
        if not self.result[-1]["res"]:
            raise self.AssertFail()

    def assert_equal(self, v1, v2, msg=''):
        self.assert_util(lambda a, b: a == b, "=", msg, v1, v2)

    def assert_not_equal(self, v1, v2, msg=''):
        self.assert_util(lambda a, b: a != b, "!=", msg, v1, v2)

    def assert_great(self, v1, v2, msg=''):
        self.assert_util(lambda a, b: a > b, ">", msg, v1, v2)
    
    def assert_ge(self, v1, v2, msg=''):
        self.assert_util(lambda a, b: a >= b, ">=", msg, v1, v2)

    def assert_in_str(self, v1, v2, msg=''):
        def _fun(a: str, b: List[str]):
            pattern = re.compile(a)
            for line in b:
                if re.search(pattern, line) is not None:
                    return True
            return False
        self.assert_util(_fun, "in", msg, v1, v2)

    def assert_in(self, v1, v2, msg=''):
        self.assert_util(lambda a, b: a in b, "in", msg, v1, v2)

    def start(self, data):
        self.result = []
        try:
            self.test(data)
        except Exception:
            pass
    
    def get_result(self):
        return {
            "name": self.name,
            # "results": self.result,
            "all": self.count,
            "pass": len([x for x in self.result if x['res']]),
            "score": len([x for x in self.result if x['res']]),
        }

class test_brk(TestBase):
    def __init__(self):
        super().__init__("brk", 3)

    def test(self, data):
        self.assert_ge(len(data), 3)
        p1 = "Before alloc,heap pos: (.+)"
        p2 = "After alloc,heap pos: (.+)"
        p3 = "Alloc again,heap pos: (.+)"
        line1 = re.findall(p1, data[0])
        line2 = re.findall(p2, data[1])
        line3 = re.findall(p3, data[2])
        if line1 == [] or line2 == [] or line3 == []:
            return
        a1 = int(line1[0], 10)
        a2 = int(line2[0], 10)
        a3 = int(line3[0], 10)
        self.assert_equal(a1 + 64, a2)
        self.assert_equal(a2 + 64, a3)

class test_chdir(TestBase):
    def __init__(self):
        super().__init__("chdir", 3)

    def test(self, data):
        self.assert_ge(len(data), 2)
        p1 = r"chdir ret: (\d)+"
        r1 = re.findall(p1, data[0])
        if r1:
            self.assert_equal(r1[0], "0")
        self.assert_in("test_chdir", data[1])

class test_clone(TestBase):
    def __init__(self):
        super().__init__("clone", 4)

    def test(self, data):
        self.assert_ge(len(data), 3)
        self.assert_in_str("  Child says successfully!", data)
        self.assert_in_str(r"pid:\d+", data)
        self.assert_in_str("clone process successfully.", data)

class test_close(TestBase):
    def __init__(self):
        super().__init__("close", 2)

    def test(self, data):
        self.assert_ge(len(data), 1)
        self.assert_in_str(r"  close \d+ success.", data)

class test_dup2(TestBase):
    def __init__(self):
        super().__init__("dup2", 2)

    def test(self, data):
        self.assert_ge(len(data), 1)
        self.assert_equal("  from fd 100", data[0])

class test_dup(TestBase):
    def __init__(self):
        super().__init__("dup", 2)

    def test(self, data):
        self.assert_ge(len(data), 1)
        res = re.findall(r"  new fd is (\d+).", data[0])
        if res:
            new_fd = int(res[0])
            self.assert_not_equal(new_fd, 1)

class test_execve(TestBase):
    def __init__(self):
        super().__init__("execve", 3)

    def test(self, data):
        self.assert_ge(len(data), 2)
        self.assert_equal("  I am test_echo.", data[0])
        self.assert_equal("execve success.", data[1])

class test_exit(TestBase):
    def __init__(self):
        super().__init__("exit", 2)

    def test(self, data):
        self.assert_ge(len(data), 1)
        self.assert_equal("exit OK.", data[0])

class test_fork(TestBase):
    def __init__(self):
        super().__init__("fork", 3)

    def test(self, data):
        self.assert_ge(len(data), 2)
        self.assert_in_str(r"  parent process\. wstatus:\d+", data)
        self.assert_in_str("  child process", data)

class test_fstat(TestBase):
    def __init__(self):
        super().__init__("fstat", 3)

    def test(self, data):
        self.assert_ge(len(data), 2)
        res = re.findall(r"fstat ret: (\d+)", data[0])
        if res:
            self.assert_equal(res[0], "0")
        res = re.findall(r"fstat: dev: \d+, inode: \d+, mode: (\d+), nlink: (\d+), size: \d+, atime: \d+, mtime: \d+, ctime: \d+", data[1])
        if res:
            self.assert_equal(res[0][1], "1")

class test_getcwd(TestBase):
    def __init__(self):
        super().__init__("getcwd", 2)

    def test(self, data):
        self.assert_ge(len(data), 1)
        self.assert_in_str("getcwd: (.+) successfully!", data)

class test_getdents(TestBase):
    def __init__(self):
        super().__init__("getdents", 5)

    def test(self, data):
        self.assert_ge(len(data), 4)
        r = re.findall(r"open fd:(\d+)", data[0])
        if r:
            self.assert_great(int(r[0]), 1)
        r = re.findall(r"getdents fd:(\d+)", data[1])
        if r:
            self.assert_great(int(r[0]), 1)
        self.assert_equal("getdents success.", data[2])
        self.assert_ge(len(data[3]), 1)
        
class test_getpid(TestBase):
    def __init__(self):
        super().__init__("getpid", 3)

    def test(self, data):
        self.assert_ge(len(data), 2)
        self.assert_equal(data[0], "getpid success.")
        r = re.findall(r"pid = (\d+)", data[1])
        if r:
            self.assert_great(int(r[0]), 0)

class test_getppid(TestBase):
    def __init__(self):
        super().__init__("getppid", 2)

    def test(self, data):
        self.assert_ge(len(data), 1)
        self.assert_in("  getppid success. ppid : ", data[0])

class test_gettimeofday(TestBase):
    def __init__(self):
        super().__init__("gettimeofday", 3)

    def test(self, data):
        self.assert_ge(len(data), 3)
        self.assert_equal("gettimeofday success.", data[0])
        res = re.findall(r"interval: (\d+)", data[2])
        if res:
            self.assert_great(int(res[0]), 0)

class test_mkdir(TestBase):
    def __init__(self):
        super().__init__("mkdir", 3)

    def test(self, data):
        self.assert_ge(len(data), 2)
        self.assert_in("mkdir ret:", data[0])
        self.assert_in("  mkdir success.", data[1])

class test_mmap(TestBase):
    def __init__(self):
        super().__init__("mmap", 3)

    def test(self, data):
        self.assert_ge(len(data), 2)
        r = re.findall(r"file len: (\d+)", data[0])
        if r:
            self.assert_ge(int(r[0]), 27)
        self.assert_equal("mmap content:   Hello, mmap successfully!", data[1])

class test_mount(TestBase):
    def __init__(self):
        super().__init__("mount", 5)

    def test(self, data):
        self.assert_ge(len(data), 4)
        r = re.findall(r"Mounting dev:(.+) to ./mnt", data[0])
        self.assert_equal(len(r) > 0, True)
        self.assert_equal(data[1], "mount return: 0")
        self.assert_equal(data[2], "mount successfully")
        self.assert_equal(data[3], "umount return: 0")

class test_munmap(TestBase):
    def __init__(self):
        super().__init__("munmap", 4)

    def test(self, data):
        self.assert_ge(len(data), 3)
        r = re.findall(r"file len: (\d+)", data[0])
        if r:
            self.assert_ge(int(r[0]), 27)
        self.assert_equal(data[1], "munmap return: 0")
        self.assert_equal(data[2], "munmap successfully!")

class test_open(TestBase):
    def __init__(self):
        super().__init__("open", 3)

    def test(self, data):
        self.assert_ge(len(data), 2)
        self.assert_equal("Hi, this is a text file.", data[0])
        self.assert_equal("syscalls testing success!", data[1])

class test_openat(TestBase):
    def __init__(self):
        super().__init__("openat", 4)

    def test(self, data):
        self.assert_ge(len(data), 3)
        r = re.findall(r"open dir fd: (\d+)", data[0])
        if r:
            self.assert_great(int(r[0]), 1)
        r1 = re.findall(r"openat fd: (\d+)", data[1])
        if r1:
            self.assert_great(int(r1[0]), int(r[0]))
        self.assert_equal(data[2], "openat success.")

class test_pipe(TestBase):
    def __init__(self):
        super().__init__("pipe", 2)

    def test(self, data):
        self.assert_ge(len(data), 3)
        # cpid0 = False
        # cpid1 = False
        # for line in data[:3]:
        #     if line == "cpid: 0":
        #         cpid0 = True
        #         continue
        #     r = re.findall(r"cpid: (\d+)", line)
        #     if r and int(r[0]) > 0:
        #         cpid1 = True
        #         continue
        # self.assert_equal(cpid0, True)
        # self.assert_equal(cpid1, True)
        self.assert_equal(data[2], "  Write to pipe successfully.")

class test_read(TestBase):
    def __init__(self):
        super().__init__("read", 3)

    def test(self, data):
        self.assert_ge(len(data), 2)
        self.assert_equal("Hi, this is a text file.", data[0])
        self.assert_equal("syscalls testing success!", data[1])

class test_sleep(TestBase):
    def __init__(self):
        super().__init__("sleep", 2)

    def test(self, data):
        self.assert_ge(len(data), 1)
        self.assert_equal(data[0], "sleep success.")

class test_times(TestBase):
    def __init__(self):
        super().__init__("times", 6)

    def test(self, data):
        self.assert_ge(len(data), 2)
        self.assert_equal(data[0], "mytimes success")
        r = re.findall(r"\{tms_utime:(.+), tms_stime:(.+), tms_cutime:(.+), tms_cstime:(.+)}", data[1])
        if r:
            self.assert_ge(int(r[0][0]), 0)
            self.assert_ge(int(r[0][1]), 0)
            self.assert_ge(int(r[0][2]), 0)
            self.assert_ge(int(r[0][3]), 0)

class test_umount(TestBase):
    def __init__(self):
        super().__init__("umount", 5)

    def test(self, data):
        self.assert_ge(len(data), 4)
        # self.assert_equal(data[0], "Mounting dev:/dev/vda2 to ./mnt")
        r = re.findall(r"Mounting dev:(.+) to ./mnt", data[0])
        self.assert_equal(len(r) > 0, True)
        self.assert_equal("mount return: 0", data[1])
        self.assert_equal("umount success.", data[2])
        self.assert_equal("return: 0", data[3])

class test_uname(TestBase):
    def __init__(self):
        super().__init__("uname", 2)

    def test(self, data):
        self.assert_ge(len(data), 1)
        self.assert_in("Uname: ", data[0])

class test_unlink(TestBase):
    def __init__(self):
        super().__init__("unlink", 2)

    def test(self, data):
        self.assert_ge(len(data), 1)
        self.assert_equal(data[0], "  unlink success!")

class test_wait(TestBase):
    def __init__(self):
        super().__init__("wait", 4)

    def test(self, data):
        self.assert_ge(len(data), 3)
        self.assert_equal(data[0], "This is child process")
        self.assert_equal(data[1], "wait child success.")
        self.assert_equal(data[2], "wstatus: 0")

class test_waitpid(TestBase):
    def __init__(self):
        super().__init__("waitpid", 4)

    def test(self, data):
        self.assert_ge(len(data), 3)
        self.assert_equal(data[0], "This is child process")
        self.assert_equal(data[1], "waitpid successfully.")
        self.assert_equal(data[2], "wstatus: 3")


class test_write(TestBase):
    def __init__(self):
        super().__init__("write", 2)

    def test(self, data):
        self.assert_ge(len(data), 1)
        self.assert_equal(data[0], "Hello operating system contest.")

class test_yield(TestBase):
    def __init__(self):
        super().__init__("yield", 4)

    def test(self, data):
        self.assert_equal(len(data), 15)
        lst = ''.join(data)
        cnt = {'0': 0, '1': 0, '2': 0, '3': 0, '4': 0}
        for c in lst:
            if c not in ('0', '1', '2', '3', '4'):
                continue
            cnt[c] += 1
        self.assert_ge(cnt['0'], 3)
        self.assert_ge(cnt['1'], 3)
        self.assert_ge(cnt['2'], 3)
        


# BBBBBBBBBB [1/5]
# CCCCCCCCCC [1/5]
# BBBBBBBBBB [2/5]
# CCCCCCCCCC [2/5]
# AAAAAAAAAA [1/5]
# CCCCCCCCCC [3/5]
# BBBBBBBBBB [3/5]
# AAAAAAAAAA [2/5]
# BBBBBBBBBB [4/5]
# CCCCCCCCCC [4/5]
# AAAAAAAAAA [3/5]
# BBBBBBBBBB [5/5]
# CCCCCCCCCC [5/5]
# AAAAAAAAAA [4/5]
# AAAAAAAAAA [5/5]

tests = [x for x in TestBase.__subclasses__()]

runner = {x.__name__: x() for x in tests}

def get_runner(name):
    # return runner.get(name, runner.get("test_"+name, runner[name+"_test"]))
    return runner.get(name, None)
# print(runner)


# TODO: Add more commands to test here
target_testcases = [
    "test_brk",
    "test_chdir",
    "test_execve",
    "test_pipe",
    "test_close",
    "test_dup",
    "test_dup2",
    "test_fstat",
    "test_getcwd",
    "test_mkdir",
    "test_open",
    "test_read",
    "test_unlink",
    "test_write",
    "test_openat",
    "test_getdents",
    "test_mount",
    "test_umount",
]

if __name__ == '__main__':
    serial_out = sys.stdin.readlines()

    test_name = None
    state = 0
    data = []
    pat = re.compile(r"========== START (.+) ==========")
    for line in serial_out:
        if line in ('', '\n'):
            continue
        if state == 0:
            # 寻找测试样例开头
            if pat.findall(line):
                test_name = pat.findall(line)[0]
                if test_name not in target_testcases:
                    continue
                # test_name = line.replace("=", '').replace(" ", "").replace("START", "")

                if data:
                    # 只找到了开头没找到结尾，说明某个样例内部使用assert提前退出
                    r = get_runner(test_name)
                    if r:
                        r.start(data)
                data = []
                state = 1
        elif state == 1:
            if "========== END " in line:
                # 测试样例结尾
                r = get_runner(test_name)
                if r:
                    r.start(data)
                state = 0
                data = []
                continue
            elif pat.findall(line):
                data = []
                test_name = pat.findall(line)[0]
                # test_name = line.replace("=", '').replace(" ", "").replace("START", "")
                continue
            # 测试样例中间
            data.append(line.replace('\n', '').replace('\r', ''))
    test_results = [x.get_result() for x in runner.values()]
    for x in runner.values():
        result = x.get_result()
        if result['all'] != result['pass'] and result['name'] in target_testcases:
            print(result['name'] + " failed!")
            exit(255)
    print("Basic testcases passed.")
    print(json.dumps(test_results))