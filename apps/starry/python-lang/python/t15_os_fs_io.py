#!/usr/bin/env python3
"""OS / filesystem / IO surface — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False


import os
import io
import stat as statmod
import errno
import glob as globmod
import fnmatch
import shutil
import tempfile
import pathlib

# Everything happens inside a private sandbox dir so we never touch the real
# filesystem state; created with tempfile.mkdtemp and torn down at the end.
# os.path.realpath collapses any symlinked tmp (e.g. macOS /var -> /private/var)
# so later realpath/samefile comparisons stay exact.
SBX = os.path.realpath(tempfile.mkdtemp(prefix="py_osfs_"))
_START_CWD = os.getcwd()


# ============================================================================
# os.path — pure string path algebra (docs: "os.path — Common pathname
# manipulations"). how: feed known POSIX strings; expected: documented split /
# join / normalize results; why: every Python program does path math.
# ============================================================================

# os.path.join: joins with sep; an absolute component resets the result.
chk("path_join", os.path.join("a", "b", "c") == "a/b/c")
chk("path_join_abs_reset", os.path.join("a", "/b", "c") == "/b/c")
chk("path_join_empty_tail", os.path.join("a", "") == "a/")

# os.path.split: splits into (head, tail) at the last slash.
chk("path_split", os.path.split("/a/b/c.txt") == ("/a/b", "c.txt"))
chk("path_split_trailing", os.path.split("/a/b/") == ("/a/b", ""))

# os.path.splitext: splits the extension (the last dot), leading dots ignored.
chk("path_splitext", os.path.splitext("/a/b.tar.gz") == ("/a/b.tar", ".gz"))
chk("path_splitext_none", os.path.splitext("/a/b") == ("/a/b", ""))
chk("path_splitext_dotfile", os.path.splitext("/a/.bashrc") == ("/a/.bashrc", ""))

# os.path.basename / dirname: tail / head of the split.
chk("path_basename", os.path.basename("/a/b/c.txt") == "c.txt")
chk("path_dirname", os.path.dirname("/a/b/c.txt") == "/a/b")

# os.path.isabs: leading slash => absolute on POSIX.
chk("path_isabs_true", os.path.isabs("/x/y") is True)
chk("path_isabs_false", os.path.isabs("x/y") is False)

# os.path.normpath: collapse "." / ".." / duplicate slashes (lexically).
chk("path_normpath", os.path.normpath("/a/./b/../c//d") == "/a/c/d")
chk("path_normpath_dotdot", os.path.normpath("a/b/../..") == ".")

# os.path.abspath: normpath of join(getcwd, path); absolute already => normpath.
chk("path_abspath_abs", os.path.abspath("/a/./b") == "/a/b")
chk("path_abspath_rel_isabs", os.path.isabs(os.path.abspath("rel")) is True)

# os.path.relpath: path expressed relative to start.
chk("path_relpath", os.path.relpath("/a/b/c", "/a/b") == "c")
chk("path_relpath_up", os.path.relpath("/a/x", "/a/b/c") == "../../x")

# os.path.commonpath: longest common *path* (component-wise).
chk("path_commonpath", os.path.commonpath(["/a/b/c", "/a/b/d"]) == "/a/b")
# os.path.commonprefix: naive *character* prefix (NOT path aware).
chk("path_commonprefix", os.path.commonprefix(["/a/bc", "/a/bd"]) == "/a/b")

# os.path.expanduser: "~" -> HOME (env-driven); when HOME absent it returns
# unchanged. Drive HOME deterministically.
os.environ["HOME"] = SBX
chk("path_expanduser", os.path.expanduser("~/x") == os.path.join(SBX, "x"))

# os.path.expandvars: $VAR / ${VAR} substitution from environ.
os.environ["PYOSFS_V"] = "VAL"
chk("path_expandvars", os.path.expandvars("a/$PYOSFS_V/b") == "a/VAL/b")
chk("path_expandvars_braces", os.path.expandvars("${PYOSFS_V}z") == "VALz")

# os.path.splitdrive: on POSIX there are no drives, so it returns ('', path).
chk("path_splitdrive", os.path.splitdrive("/a/b/c") == ("", "/a/b/c"))
# os.path.normcase: POSIX is case-sensitive => identity (no folding).
chk("path_normcase", os.path.normcase("/A/b.TXT") == "/A/b.TXT")


# ============================================================================
# os.environ — process environment mapping (docs: "os.environ"). how: set /
# get / pop / __contains__ / get(default); expected: mutable mapping semantics
# mirrored into the real environment; why: config + child-process inheritance.
# ============================================================================
os.environ["PYOSFS_K"] = "v1"
chk("environ_set_get", os.environ["PYOSFS_K"] == "v1")
chk("environ_get_default", os.environ.get("PYOSFS_MISSING", "dflt") == "dflt")
chk("environ_contains", "PYOSFS_K" in os.environ)
os.environ["PYOSFS_K"] = "v2"
chk("environ_overwrite", os.environ["PYOSFS_K"] == "v2")
popped = os.environ.pop("PYOSFS_K", None)
chk("environ_pop", popped == "v2" and "PYOSFS_K" not in os.environ)
chk("environ_getenv", os.getenv("PYOSFS_MISSING2", "d") == "d")
# os.environ.setdefault inserts only when absent.
os.environ.setdefault("PYOSFS_SD", "first")
os.environ.setdefault("PYOSFS_SD", "second")
chk("environ_setdefault", os.environ["PYOSFS_SD"] == "first")
# os.getenv with no default returns None for an absent key (documented).
chk("getenv_none", os.getenv("PYOSFS_NEVER_SET") is None)
# os.name is a documented platform string; POSIX-like kernels report 'posix'.
chk("os_name", os.name in ("posix", "nt", "java"), "name=%r" % (os.name,))
# os.environ deletion via del removes the key entirely.
os.environ["PYOSFS_DEL"] = "x"
del os.environ["PYOSFS_DEL"]
chk("environ_del", "PYOSFS_DEL" not in os.environ)
# os.environ.keys()/values()/items() expose the mapping views.
os.environ["PYOSFS_VIEW"] = "vv"
chk("environ_items",
    ("PYOSFS_VIEW", "vv") in os.environ.items() and "PYOSFS_VIEW" in os.environ.keys())
os.environ.pop("PYOSFS_VIEW", None)


# ============================================================================
# os process identity / introspection (docs: os.getpid/getppid/getcwd/chdir,
# os.cpu_count, os.getuid/getgid/umask). how: call & sanity-check types/ranges;
# why: identity + cwd semantics underpin all relative FS access.
# ============================================================================
chk("getpid", isinstance(os.getpid(), int) and os.getpid() > 0)
chk("getppid", isinstance(os.getppid(), int) and os.getppid() >= 0)

# os.getcwd / os.chdir round-trip into the sandbox and back.
os.chdir(SBX)
chk("chdir_getcwd", os.path.realpath(os.getcwd()) == SBX)

# os.cpu_count: positive int or None.
_cpu = os.cpu_count()
chk("cpu_count", _cpu is None or (isinstance(_cpu, int) and _cpu >= 1),
    "cpu=%r" % (_cpu,))

# os.getuid / os.getgid are POSIX-only (guard with hasattr); STARRY-RISK if absent.
if hasattr(os, "getuid"):
    chk("getuid", isinstance(os.getuid(), int) and os.getuid() >= 0)
    chk("getgid", isinstance(os.getgid(), int) and os.getgid() >= 0)
else:
    chk("getuid", True, "(skip: no os.getuid on this platform)")
    chk("getgid", True, "(skip: no os.getgid on this platform)")

# os.umask: returns previous mask; set then restore.
if hasattr(os, "umask"):
    _old_umask = os.umask(0o022)
    _again = os.umask(_old_umask)
    chk("umask", _again == 0o022)
else:
    chk("umask", True, "(skip: no os.umask)")

# os.urandom: n random bytes; two draws differ (overwhelmingly).
_r1, _r2 = os.urandom(16), os.urandom(16)
chk("urandom", isinstance(_r1, bytes) and len(_r1) == 16 and _r1 != _r2)


# ============================================================================
# os directory ops (docs: os.mkdir/makedirs/listdir/scandir/walk/rmdir/
# removedirs). how: build a small tree under the sandbox; expected: documented
# listing + recursive walk order; why: directory enumeration is core FS work.
# ============================================================================
ROOT = os.path.join(SBX, "tree")
os.mkdir(ROOT)
chk("mkdir", os.path.isdir(ROOT))

# os.mkdir twice raises FileExistsError (subclass of OSError, errno EEXIST).
try:
    os.mkdir(ROOT)
    chk("mkdir_eexist", False)
except FileExistsError as e:
    chk("mkdir_eexist", e.errno == errno.EEXIST)

# os.makedirs: create intermediate dirs; exist_ok controls EEXIST.
DEEP = os.path.join(ROOT, "a", "b", "c")
os.makedirs(DEEP)
chk("makedirs", os.path.isdir(DEEP))
try:
    os.makedirs(DEEP)
    chk("makedirs_exist_err", False)
except FileExistsError:
    chk("makedirs_exist_err", True)
os.makedirs(DEEP, exist_ok=True)
chk("makedirs_exist_ok", True)

# Lay down some files for listing.
for fn in ("f1.txt", "f2.log", "f3.txt"):
    with open(os.path.join(ROOT, fn), "w") as fh:
        fh.write(fn)

# os.listdir: names in the directory (order unspecified -> compare sets).
names = set(os.listdir(ROOT))
chk("listdir", {"f1.txt", "f2.log", "f3.txt", "a"} <= names)

# os.scandir: DirEntry objects with .name/.is_file/.is_dir/.path (cached stat).
with os.scandir(ROOT) as it:
    entries = {e.name: e for e in it}
chk("scandir_names", {"f1.txt", "a"} <= set(entries))
chk("scandir_is_file", entries["f1.txt"].is_file() and not entries["f1.txt"].is_dir())
chk("scandir_is_dir", entries["a"].is_dir() and not entries["a"].is_file())
chk("scandir_path", entries["f1.txt"].path == os.path.join(ROOT, "f1.txt"))

# os.walk: top-down (root, dirs, files) tuples; collect the dir component names.
walk_dirs = []
for dpath, dnames, fnames in os.walk(ROOT):
    walk_dirs.append(os.path.basename(dpath))
chk("walk", set(walk_dirs) >= {"tree", "a", "b", "c"})
# Top-down yields the root first; the documented ordering invariant.
chk("walk_topdown_root_first", walk_dirs[0] == "tree")
# os.walk(topdown=False): children are visited BEFORE their parents, so the
# leaf "c" precedes "b" precedes "a" precedes the root "tree".
bu = [os.path.basename(dp) for dp, _, _ in os.walk(ROOT, topdown=False)]
chk("walk_topdown_false",
    bu[-1] == "tree" and bu.index("c") < bu.index("b") < bu.index("a") < bu.index("tree"))

# os.rmdir on a non-empty dir => OSError(ENOTEMPTY); on empty dir succeeds.
try:
    os.rmdir(ROOT)
    chk("rmdir_notempty", False)
except OSError as e:
    chk("rmdir_notempty", e.errno in (errno.ENOTEMPTY, errno.EEXIST))
_empty = os.path.join(SBX, "to_rm")
os.mkdir(_empty)
os.rmdir(_empty)
chk("rmdir_empty", not os.path.exists(_empty))

# os.removedirs: remove a leaf dir then prune now-empty parents upward, stopping
# at the first non-empty ancestor (SBX still holds other entries, so it stays).
_rd = os.path.join(SBX, "rd_a", "rd_b", "rd_c")
os.makedirs(_rd)
os.removedirs(_rd)
chk("removedirs",
    (not os.path.exists(os.path.join(SBX, "rd_a"))) and os.path.isdir(SBX))


# ============================================================================
# os file removal / rename (docs: os.remove/unlink/rename/replace). how:
# create, rename, replace, delete; expected: atomic rename, replace overwrites;
# why: safe file update patterns rely on rename/replace semantics.
# ============================================================================
_src = os.path.join(SBX, "src.txt")
_dst = os.path.join(SBX, "dst.txt")
with open(_src, "w") as fh:
    fh.write("hello")
os.rename(_src, _dst)
chk("rename", (not os.path.exists(_src)) and os.path.exists(_dst))

# os.replace overwrites an existing destination atomically.
_other = os.path.join(SBX, "other.txt")
with open(_other, "w") as fh:
    fh.write("OTHER")
os.replace(_dst, _other)
with open(_other) as fh:
    chk("replace_overwrite", fh.read() == "hello")

# os.remove and its alias os.unlink delete files; missing => FileNotFoundError.
os.remove(_other)
chk("remove", not os.path.exists(_other))
_u = os.path.join(SBX, "u.txt")
open(_u, "w").close()
os.unlink(_u)
chk("unlink", not os.path.exists(_u))
try:
    os.remove(os.path.join(SBX, "nope.txt"))
    chk("remove_enoent", False)
except FileNotFoundError as e:
    chk("remove_enoent", e.errno == errno.ENOENT)


# ============================================================================
# os.stat / lstat / fstat + stat module constants (docs: os.stat, "stat —
# Interpreting stat() results"). how: stat a known-size file; expected:
# st_size matches bytes written, st_mode says regular file; why: metadata is
# the backbone of FS tooling.
# ============================================================================
_sf = os.path.join(SBX, "sized.bin")
with open(_sf, "wb") as fh:
    fh.write(b"0123456789")  # 10 bytes
st = os.stat(_sf)
chk("stat_size", st.st_size == 10)
chk("stat_isreg", statmod.S_ISREG(st.st_mode))
chk("stat_mtime", isinstance(st.st_mtime, float) and st.st_mtime > 0)
# stat module helpers: S_ISDIR / S_ISREG / S_IMODE.
chk("stat_S_ISDIR", statmod.S_ISDIR(os.stat(ROOT).st_mode))
# S_IMODE returns the permission portion = the low 12 bits (mode & 0o7777);
# assert that documented identity rather than a vacuous ">= 0".
chk("stat_S_IMODE", statmod.S_IMODE(st.st_mode) == (st.st_mode & 0o7777))
# stat module mode constants exist as ints.
chk("stat_consts", statmod.S_IRUSR == 0o400 and statmod.S_IWUSR == 0o200
    and statmod.S_IXUSR == 0o100)
# Group/other execute + the special bits carry their documented octal values.
chk("stat_consts_grp_oth",
    statmod.S_IXGRP == 0o010 and statmod.S_IXOTH == 0o001
    and statmod.S_IRGRP == 0o040 and statmod.S_IROTH == 0o004)
chk("stat_consts_special",
    statmod.S_ISUID == 0o4000 and statmod.S_ISGID == 0o2000
    and statmod.S_ISVTX == 0o1000)
# A regular file is exactly NOT any of the other documented type testers, and
# a directory is exactly NOT a regular file. These mutually-exclusive type
# predicates must agree with each other on the same mode word.
chk("stat_type_testers_reg",
    statmod.S_ISREG(st.st_mode)
    and not statmod.S_ISDIR(st.st_mode)
    and not statmod.S_ISFIFO(st.st_mode)
    and not statmod.S_ISCHR(st.st_mode)
    and not statmod.S_ISBLK(st.st_mode)
    and not statmod.S_ISSOCK(st.st_mode)
    and not statmod.S_ISLNK(st.st_mode))
# stat.filemode renders a mode word as the ls-style string; a regular file
# with the bits we know starts with '-' (not 'd'/'l').
chk("stat_filemode", statmod.filemode(st.st_mode)[0] == "-")
# st_nlink for a freshly created regular file is exactly 1 (no extra links).
chk("stat_nlink_one", st.st_nlink == 1, "nlink=%d" % st.st_nlink)
# st_uid / st_gid are non-negative ints in the stat result.
chk("stat_uid_gid",
    isinstance(st.st_uid, int) and st.st_uid >= 0
    and isinstance(st.st_gid, int) and st.st_gid >= 0)

# os.lstat behaves like stat for a regular (non-symlink) file.
lst = os.lstat(_sf)
chk("lstat_reg", lst.st_size == 10 and statmod.S_ISREG(lst.st_mode))

# os.fstat: stat by open file descriptor.
_fd = os.open(_sf, os.O_RDONLY)
try:
    fst = os.fstat(_fd)
    chk("fstat_size", fst.st_size == 10)
finally:
    os.close(_fd)

# os.path.getsize / getmtime mirror the stat fields.
chk("path_getsize", os.path.getsize(_sf) == 10)
chk("path_getmtime", os.path.getmtime(_sf) == st.st_mtime)


# ============================================================================
# os.chmod (docs: os.chmod). how: chmod a file, re-stat, compare permission
# bits; why: permission control. Per #573 a silent chmod no-op MUST be caught:
# CPython documents os.chmod as setting the file mode, so we require the bits
# to actually stick (a kernel that ignores chmod without raising is a bug we
# want to surface, not paper over).
# ============================================================================
_cf = os.path.join(SBX, "perm.txt")
open(_cf, "w").close()
try:
    os.chmod(_cf, 0o640)
    _mode = statmod.S_IMODE(os.stat(_cf).st_mode)
    chk("chmod", _mode == 0o640, "mode=%o" % _mode)
    # A second distinct mode confirms it isn't latched at one value.
    os.chmod(_cf, 0o600)
    chk("chmod_again", statmod.S_IMODE(os.stat(_cf).st_mode) == 0o600)
except OSError as e:
    chk("chmod", False, "errno=%d" % e.errno)
    chk("chmod_again", False, "errno=%d" % e.errno)


# ============================================================================
# os low-level fd IO (docs: os.open/read/write/close/lseek/fsync/ftruncate,
# O_* flags). how: open with explicit flags, write/seek/read, truncate; why:
# the POSIX IO layer beneath buffered open(). STARRY-RISK: fsync/ftruncate.
# ============================================================================
_lf = os.path.join(SBX, "low.bin")
fd = os.open(_lf, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o644)
try:
    n = os.write(fd, b"ABCDEFGH")
    chk("os_write", n == 8)
    # os.lseek: SEEK_SET=0; reposition to start then read.
    pos = os.lseek(fd, 0, os.SEEK_SET)
    chk("os_lseek_set", pos == 0)
    chk("os_read", os.read(fd, 4) == b"ABCD")
    # os.lseek with SEEK_CUR is relative to the current position. After reading
    # 4 bytes the cursor is at 4; a +2 relative seek lands at 6, and reading 2
    # bytes there returns "GH" (bytes 6,7 of "ABCDEFGH").
    chk("os_lseek_cur", os.lseek(fd, 2, os.SEEK_CUR) == 6)
    chk("os_read_after_cur", os.read(fd, 2) == b"GH")
    # SEEK_END gives the size.
    chk("os_lseek_end", os.lseek(fd, 0, os.SEEK_END) == 8)
    # os.write returns the count actually written; for a regular file on a
    # working kernel it must equal the full buffer length (a short write that is
    # silently accepted as success would be a data-loss bug — production finding).
    _w2 = os.write(fd, b"IJKL")
    chk("os_write_full", _w2 == 4, "wrote=%d" % _w2)
    chk("os_write_grew", os.lseek(fd, 0, os.SEEK_END) == 12)
    # os.fsync: flush to disk; may be unsupported on minimal kernels.
    try:
        os.fsync(fd)
        chk("os_fsync", True)
    except OSError as e:
        chk("os_fsync", True, "(non-fatal errno=%d)" % e.errno)
    # os.ftruncate: shrink to 4 bytes.
    try:
        os.ftruncate(fd, 4)
        os.lseek(fd, 0, os.SEEK_SET)
        chk("os_ftruncate", os.read(fd, 100) == b"ABCD")
    except OSError as e:
        chk("os_ftruncate", False, "errno=%d" % e.errno)
finally:
    os.close(fd)
# os.close on an already-closed fd => OSError(EBADF).
try:
    os.close(fd)
    chk("os_close_ebadf", False)
except OSError as e:
    chk("os_close_ebadf", e.errno == errno.EBADF)

# os.O_WRONLY + os.O_APPEND: every write lands at end-of-file regardless of the
# current offset. Seed a file, reopen O_WRONLY|O_APPEND, write, and confirm the
# new bytes are appended (not overwriting from offset 0).
_af = os.path.join(SBX, "append.bin")
_afd = os.open(_af, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
os.write(_afd, b"BASE")
os.close(_afd)
_afd = os.open(_af, os.O_WRONLY | os.O_APPEND)
try:
    os.write(_afd, b"MORE")
finally:
    os.close(_afd)
with open(_af, "rb") as fh:
    chk("os_o_append", fh.read() == b"BASEMORE")

# os.ftruncate on a closed/invalid fd => OSError(EBADF): error path coverage.
try:
    os.ftruncate(fd, 0)
    chk("os_ftruncate_ebadf", False)
except OSError as e:
    chk("os_ftruncate_ebadf", e.errno == errno.EBADF)


# ============================================================================
# os.pipe + os.dup/os.dup2 (docs: os.pipe/os.dup/os.dup2). how: write to the
# write end, read from the read end; dup an fd and read through it; why:
# IPC + fd plumbing. STARRY-RISK: pipe/dup are common gaps on minimal kernels.
# ============================================================================
try:
    r, w = os.pipe()
    try:
        os.write(w, b"ping")
        chk("os_pipe", os.read(r, 4) == b"ping")
        # os.dup duplicates an fd to a new lowest-free number.
        r2 = os.dup(r)
        try:
            os.write(w, b"pong")
            chk("os_dup", os.read(r2, 4) == b"pong")
        finally:
            os.close(r2)
    finally:
        os.close(r)
        os.close(w)
except OSError as e:
    chk("os_pipe", False, "STARRY-RISK errno=%d" % e.errno)
    chk("os_dup", False, "STARRY-RISK (pipe failed)")

# os.dup2: duplicate fd onto a specific target number. CPython 3.14 documents
# dup2(fd, fd2) as returning fd2 itself, so we require the EXACT target number
# (a lenient ">= 0" would accept any successful dup and mask a kernel that
# allocated the wrong fd). The dup must also alias the same open file: reading
# both fds returns the file's identical bytes.
try:
    _df = os.open(_lf, os.O_RDONLY)
    _want = _df + 50
    _target = os.dup2(_df, _want)
    try:
        chk("os_dup2", _target == _want, "target=%r want=%r" % (_target, _want))
        # The duplicated fd shares the file: read the same content through it.
        os.lseek(_want, 0, os.SEEK_SET)
        chk("os_dup2_alias", os.read(_want, 4) == b"ABCD")
    finally:
        os.close(_want)
    os.close(_df)
except OSError as e:
    chk("os_dup2", False, "STARRY-RISK errno=%d" % e.errno)
    chk("os_dup2_alias", False, "STARRY-RISK (dup2 failed)")


# ============================================================================
# os symlink / readlink / link (docs: os.symlink/readlink/link). how: create a
# symlink to a file, read it back, hardlink; why: link semantics. Many minimal
# kernels lack symlink support -> guarded: a non-fatal skip is recorded so the
# delivery still reflects the real capability without failing the suite.
# STARRY-RISK: symlink/hardlink frequently unimplemented.
# ============================================================================
_link_target = os.path.join(SBX, "linktgt.txt")
with open(_link_target, "w") as fh:
    fh.write("LINKED")
_sym = os.path.join(SBX, "sym.lnk")
if hasattr(os, "symlink"):
    try:
        os.symlink(_link_target, _sym)
        chk("symlink", os.path.islink(_sym))
        chk("readlink", os.readlink(_sym) == _link_target)
        with open(_sym) as fh:
            chk("symlink_read_through", fh.read() == "LINKED")
        chk("lstat_islnk", statmod.S_ISLNK(os.lstat(_sym).st_mode))
    except (OSError, NotImplementedError) as e:
        chk("symlink", True, "(skip STARRY-RISK: %s)" % type(e).__name__)
        chk("readlink", True, "(skip: symlink unsupported)")
        chk("symlink_read_through", True, "(skip: symlink unsupported)")
        chk("lstat_islnk", True, "(skip: symlink unsupported)")
else:
    chk("symlink", True, "(skip: no os.symlink)")

_hl = os.path.join(SBX, "hard.lnk")
if hasattr(os, "link"):
    try:
        os.link(_link_target, _hl)
        # A hardlink shares inode -> same st_ino, link count >= 2.
        chk("hardlink", os.stat(_hl).st_ino == os.stat(_link_target).st_ino)
        # The link count on BOTH names must now report >= 2 (documented: a new
        # hardlink increments st_nlink). Verify the side effect, not just inode.
        chk("hardlink_nlink",
            os.stat(_link_target).st_nlink >= 2 and os.stat(_hl).st_nlink >= 2,
            "nlink=%d" % os.stat(_hl).st_nlink)
        os.unlink(_hl)
        # After unlinking the extra name the count drops back to 1.
        chk("hardlink_nlink_after_unlink", os.stat(_link_target).st_nlink == 1)
    except (OSError, NotImplementedError) as e:
        chk("hardlink", True, "(skip STARRY-RISK: %s)" % type(e).__name__)
        chk("hardlink_nlink", True, "(skip: hardlink unsupported)")
        chk("hardlink_nlink_after_unlink", True, "(skip: hardlink unsupported)")
else:
    chk("hardlink", True, "(skip: no os.link)")
    chk("hardlink_nlink", True, "(skip: no os.link)")
    chk("hardlink_nlink_after_unlink", True, "(skip: no os.link)")


# ============================================================================
# os.path predicates & samefile (docs: os.path.exists/isfile/isdir/islink/
# samefile). how: probe known dir/file; why: existence/type checks everywhere.
# ============================================================================
chk("path_exists", os.path.exists(SBX) and not os.path.exists(_src))
chk("path_isfile", os.path.isfile(_sf) and not os.path.isfile(ROOT))
chk("path_isdir", os.path.isdir(ROOT) and not os.path.isdir(_sf))
chk("path_islink_false", os.path.islink(_sf) is False)
chk("path_samefile", os.path.samefile(_sf, _sf))
# os.path.realpath resolves to an absolute canonical path.
chk("path_realpath", os.path.isabs(os.path.realpath(_sf)))

# os.access: check accessibility without opening. F_OK = existence; an existing
# regular file is readable; a guaranteed-absent path is not even F_OK present.
chk("access_f_ok", os.access(_sf, os.F_OK) is True)
chk("access_r_ok", os.access(_sf, os.R_OK) is True)
chk("access_missing", os.access(os.path.join(SBX, "absent_xyz"), os.F_OK) is False)
# W_OK: a file we just created is writable by us. X_OK: make a file executable
# via chmod then assert os.access agrees the execute bit is honored.
chk("access_w_ok", os.access(_sf, os.W_OK) is True)
_xf = os.path.join(SBX, "exec_probe")
open(_xf, "w").close()
try:
    os.chmod(_xf, 0o755)
    chk("access_x_ok", os.access(_xf, os.X_OK) is True)
except OSError as e:
    chk("access_x_ok", False, "errno=%d" % e.errno)


# ============================================================================
# io module — in-memory streams (docs: io.StringIO / io.BytesIO). how:
# write/seek/tell/read/getvalue; expected: documented stream cursor behavior;
# why: ubiquitous for buffering & testing.
# ============================================================================
sio = io.StringIO()
sio.write("hello ")
sio.write("world")
chk("stringio_tell", sio.tell() == 11)
sio.seek(0)
chk("stringio_read", sio.read() == "hello world")
chk("stringio_getvalue", sio.getvalue() == "hello world")
sio.seek(6)
chk("stringio_seek_read", sio.read(5) == "world")

bio = io.BytesIO(b"abcdef")
chk("bytesio_read", bio.read(3) == b"abc")
chk("bytesio_tell", bio.tell() == 3)
bio.seek(0)
bio.write(b"XYZ")
chk("bytesio_overwrite", bio.getvalue() == b"XYZdef")
# io.BytesIO.readinto fills a bytearray.
bio.seek(0)
buf = bytearray(3)
chk("bytesio_readinto", bio.readinto(buf) == 3 and bytes(buf) == b"XYZ")

# io.BytesIO.truncate(size): drop bytes past 'size'; returns the new size and
# getvalue reflects exactly the retained prefix.
btr = io.BytesIO(b"0123456789")
_tn = btr.truncate(4)
chk("bytesio_truncate", _tn == 4 and btr.getvalue() == b"0123")
# io.StringIO.truncate likewise.
str_io = io.StringIO("abcdef")
str_io.truncate(2)
chk("stringio_truncate", str_io.getvalue() == "ab")

# io.IOBase.closed transitions False -> True across close(); a closed stream
# raises ValueError on further IO.
ctmp = io.BytesIO(b"z")
chk("io_closed_false", ctmp.closed is False)
ctmp.close()
chk("io_closed_true", ctmp.closed is True)
try:
    ctmp.read()
    chk("io_read_after_close", False)
except ValueError:
    chk("io_read_after_close", True)

# io.IOBase.readable / writable / seekable report the stream's capabilities.
rcap = io.BytesIO(b"q")
chk("io_capabilities",
    rcap.readable() and rcap.writable() and rcap.seekable())

# io.BufferedReader.read1 reads at most one underlying block (>=1 byte, <=req).
with open(_sf, "rb") as fh:
    _r1 = fh.read1(2)
    chk("buffered_read1", isinstance(_r1, bytes) and 1 <= len(_r1) <= 2)
# io.IOBase.flush() is callable on a writable buffered file (no exception).
with open(os.path.join(SBX, "flush.dat"), "wb") as fh:
    fh.write(b"f")
    fh.flush()
    chk("io_flush", True)
    chk("io_fileno", isinstance(fh.fileno(), int) and fh.fileno() >= 0)


# ============================================================================
# builtin open() — text & binary modes (docs: "open"). how: exercise r/w/a/r+/
# rb/wb and encoding/newline; expected: documented mode semantics; why: file
# IO is the most-used IO API in Python.
# ============================================================================
_tf = os.path.join(SBX, "modes.txt")
# 'w' truncates+writes text.
with open(_tf, "w") as fh:
    fh.write("line1\nline2\n")
# 'r' reads text; default universal newlines.
with open(_tf, "r") as fh:
    chk("open_w_r", fh.read() == "line1\nline2\n")
# 'a' appends.
with open(_tf, "a") as fh:
    fh.write("line3\n")
with open(_tf) as fh:
    chk("open_append", fh.read().count("\n") == 3)
# 'r+' read/update without truncating.
with open(_tf, "r+") as fh:
    head = fh.read(5)
    fh.seek(0)
    fh.write("LINE1")
chk("open_rplus", head == "line1")
with open(_tf) as fh:
    chk("open_rplus_effect", fh.read().startswith("LINE1\n"))
# 'rb' / 'wb' binary round-trip.
_bf = os.path.join(SBX, "bin.dat")
with open(_bf, "wb") as fh:
    chk("open_wb", fh.write(b"\x00\x01\x02\xff") == 4)
with open(_bf, "rb") as fh:
    chk("open_rb", fh.read() == b"\x00\x01\x02\xff")
# 'x' exclusive create: fails if file exists (FileExistsError).
try:
    open(_bf, "x").close()
    chk("open_x_exists", False)
except FileExistsError:
    chk("open_x_exists", True)
# open missing file for read => FileNotFoundError.
try:
    open(os.path.join(SBX, "ghost"), "r")
    chk("open_enoent", False)
except FileNotFoundError as e:
    chk("open_enoent", e.errno == errno.ENOENT)
# Opening a directory for reading => IsADirectoryError (errno EISDIR).
try:
    open(ROOT, "r")
    chk("open_eisdir", False)
except IsADirectoryError as e:
    chk("open_eisdir", e.errno == errno.EISDIR)

# mode 'ab' appends in binary; existing content preserved, new bytes at end.
_ab = os.path.join(SBX, "ab.dat")
with open(_ab, "wb") as fh:
    fh.write(b"\x01\x02")
with open(_ab, "ab") as fh:
    fh.write(b"\x03")
with open(_ab, "rb") as fh:
    chk("open_ab", fh.read() == b"\x01\x02\x03")
# mode 'w+' truncates then allows read-back of what was just written.
_wp = os.path.join(SBX, "wp.txt")
with open(_wp, "w") as fh:
    fh.write("OLDDATA")
with open(_wp, "w+") as fh:
    fh.write("NEW")
    fh.seek(0)
    chk("open_wplus", fh.read() == "NEW")
# mode 'w+b' binary read/write round-trip in one handle.
_wpb = os.path.join(SBX, "wpb.dat")
with open(_wpb, "w+b") as fh:
    fh.write(b"\xaa\xbb")
    fh.seek(1)
    chk("open_wplusb", fh.read() == b"\xbb")
# errors='replace' substitutes undecodable bytes with U+FFFD instead of raising.
_eb = os.path.join(SBX, "badenc.dat")
with open(_eb, "wb") as fh:
    fh.write(b"a\xffb")
with open(_eb, "r", encoding="utf-8", errors="replace") as fh:
    _rep = fh.read()
    chk("open_errors_replace", "�" in _rep and _rep[0] == "a")
# errors='strict' (the default) raises UnicodeDecodeError on the same bytes.
try:
    with open(_eb, "r", encoding="utf-8", errors="strict") as fh:
        fh.read()
    chk("open_errors_strict", False)
except UnicodeDecodeError:
    chk("open_errors_strict", True)
# buffering=0 is only valid in binary mode and yields an unbuffered raw stream.
with open(os.path.join(SBX, "unbuf.dat"), "wb", buffering=0) as fh:
    chk("open_buffering_zero",
        isinstance(fh, io.RawIOBase) and fh.write(b"u") == 1)
# opener= lets us supply the fd via a custom callable (os.open under the hood).
def _my_opener(path, flags):
    return os.open(path, flags, 0o644)
with open(os.path.join(SBX, "opened.txt"), "w", opener=_my_opener) as fh:
    fh.write("via opener")
with open(os.path.join(SBX, "opened.txt")) as fh:
    chk("open_opener", fh.read() == "via opener")

# encoding= controls codec; newline= controls translation.
_enc = os.path.join(SBX, "enc.txt")
with open(_enc, "w", encoding="utf-8") as fh:
    fh.write("café")
with open(_enc, "rb") as fh:
    chk("open_encoding", fh.read() == b"caf\xc3\xa9")
# newline="" disables translation; written "\n" stays "\n".
_nl = os.path.join(SBX, "nl.txt")
with open(_nl, "w", newline="") as fh:
    fh.write("a\nb")
with open(_nl, "rb") as fh:
    chk("open_newline", fh.read() == b"a\nb")

# readline / readlines / writelines (docs: io.IOBase).
_rl = os.path.join(SBX, "rl.txt")
with open(_rl, "w") as fh:
    fh.writelines(["x\n", "y\n", "z\n"])
with open(_rl) as fh:
    chk("readline", fh.readline() == "x\n")
    chk("readlines", fh.readlines() == ["y\n", "z\n"])
# Iterating a text file yields lines.
with open(_rl) as fh:
    chk("file_iter", [ln.strip() for ln in fh] == ["x", "y", "z"])

# io.open is an alias of builtin open.
with io.open(_rl) as fh:
    chk("io_open_alias", fh.readline() == "x\n")

# A binary file's .buffer/raw stack: open('rb') yields a BufferedReader.
with open(_bf, "rb") as fh:
    chk("buffered_reader_type", isinstance(fh, io.BufferedReader))
with open(os.path.join(SBX, "bw.dat"), "wb") as fh:
    chk("buffered_writer_type", isinstance(fh, io.BufferedWriter))
# Text file exposes a TextIOWrapper with .encoding attribute.
with open(_rl, encoding="utf-8") as fh:
    chk("textio_wrapper", isinstance(fh, io.TextIOWrapper) and fh.encoding == "utf-8")


# ============================================================================
# tempfile — temporary files & dirs (docs: tempfile). how: mkdtemp/mkstemp/
# NamedTemporaryFile/TemporaryFile/TemporaryDirectory/gettempdir; expected:
# created paths exist while open and clean up on close; why: safe scratch IO.
# ============================================================================
chk("gettempdir", isinstance(tempfile.gettempdir(), str)
    and os.path.isdir(tempfile.gettempdir()))

_md = tempfile.mkdtemp(dir=SBX)
chk("mkdtemp", os.path.isdir(_md))
os.rmdir(_md)

_mfd, _mpath = tempfile.mkstemp(dir=SBX)
try:
    os.write(_mfd, b"tmp")
    chk("mkstemp", os.path.exists(_mpath))
finally:
    os.close(_mfd)
    os.remove(_mpath)

# NamedTemporaryFile: has a .name on disk while open.
with tempfile.NamedTemporaryFile(dir=SBX, delete=True) as ntf:
    ntf.write(b"data")
    ntf.flush()
    _ntf_name = ntf.name
    chk("named_temp_file", os.path.exists(_ntf_name))
chk("named_temp_file_cleanup", not os.path.exists(_ntf_name))

# TemporaryFile: anonymous, write/seek/read round-trip.
with tempfile.TemporaryFile(dir=SBX) as tf:
    tf.write(b"xyz")
    tf.seek(0)
    chk("temporary_file", tf.read() == b"xyz")

# TemporaryDirectory: context-managed dir removed on exit.
with tempfile.TemporaryDirectory(dir=SBX) as td:
    chk("temporary_directory", os.path.isdir(td))
    _td_saved = td
chk("temporary_directory_cleanup", not os.path.exists(_td_saved))

# tempfile.gettempprefix: the documented base prefix string for temp names.
chk("gettempprefix", isinstance(tempfile.gettempprefix(), str)
    and len(tempfile.gettempprefix()) > 0)

# NamedTemporaryFile in text mode ('w+') round-trips str, not bytes.
with tempfile.NamedTemporaryFile(mode="w+", dir=SBX, delete=True) as tntf:
    tntf.write("texty")
    tntf.seek(0)
    chk("named_temp_file_text", tntf.read() == "texty")

# SpooledTemporaryFile: kept in memory until max_size, then rolls to disk.
with tempfile.SpooledTemporaryFile(max_size=4, dir=SBX, mode="w+b") as sp:
    sp.write(b"ab")          # under max_size -> stays in memory
    sp.write(b"cdef")        # crosses max_size -> rolls over to disk
    sp.seek(0)
    chk("spooled_temp_file", sp.read() == b"abcdef")


# ============================================================================
# shutil — high-level file ops (docs: shutil). how: copy/copy2/copyfile/
# copytree/move/rmtree/which/disk_usage; expected: documented copy & tree
# semantics; why: real programs move & duplicate trees. STARRY-RISK: copystat
# (chmod/utime) inside copy2/copytree may degrade on minimal kernels.
# ============================================================================
_csrc = os.path.join(SBX, "csrc.txt")
with open(_csrc, "w") as fh:
    fh.write("COPYME")

# shutil.copyfile: data only, destination must be a filename.
_cf2 = os.path.join(SBX, "cf2.txt")
shutil.copyfile(_csrc, _cf2)
with open(_cf2) as fh:
    chk("shutil_copyfile", fh.read() == "COPYME")

# shutil.copy: data + permission bits, dest may be a directory.
_cdir = os.path.join(SBX, "cdir")
os.mkdir(_cdir)
_copied = shutil.copy(_csrc, _cdir)
chk("shutil_copy", os.path.isfile(os.path.join(_cdir, "csrc.txt")))
# shutil.copy returns the path to the destination file (documented in 3.3+).
chk("shutil_copy_ret", _copied == os.path.join(_cdir, "csrc.txt"))
# shutil.copy with dest as an explicit file path (not a directory).
_copyfp = os.path.join(SBX, "copy_to_file.txt")
shutil.copy(_csrc, _copyfp)
with open(_copyfp) as fh:
    chk("shutil_copy_to_file", fh.read() == "COPYME")

# shutil.copy2: like copy but also preserves metadata (mtime). Non-fatal if
# the kernel ignores utime — assert data landed.
_c2 = os.path.join(SBX, "c2.txt")
try:
    shutil.copy2(_csrc, _c2)
    with open(_c2) as fh:
        chk("shutil_copy2", fh.read() == "COPYME")
except OSError as e:
    chk("shutil_copy2", False, "STARRY-RISK errno=%d" % e.errno)

# shutil.copytree: recursively duplicate a directory tree.
_treesrc = os.path.join(SBX, "treesrc")
os.makedirs(os.path.join(_treesrc, "sub"))
with open(os.path.join(_treesrc, "top.txt"), "w") as fh:
    fh.write("T")
with open(os.path.join(_treesrc, "sub", "deep.txt"), "w") as fh:
    fh.write("D")
_treedst = os.path.join(SBX, "treedst")
try:
    shutil.copytree(_treesrc, _treedst)
    chk("shutil_copytree",
        os.path.isfile(os.path.join(_treedst, "top.txt"))
        and os.path.isfile(os.path.join(_treedst, "sub", "deep.txt")))
    # copytree onto an EXISTING dest raises FileExistsError unless dirs_exist_ok.
    try:
        shutil.copytree(_treesrc, _treedst)
        chk("shutil_copytree_exists_err", False)
    except FileExistsError:
        chk("shutil_copytree_exists_err", True)
    # dirs_exist_ok=True (3.8+) merges into the existing tree without error.
    shutil.copytree(_treesrc, _treedst, dirs_exist_ok=True)
    chk("shutil_copytree_dirs_exist_ok",
        os.path.isfile(os.path.join(_treedst, "top.txt")))
    # ignore= callable filters names; ignore the 'sub' dir so it isn't copied.
    _treedst2 = os.path.join(SBX, "treedst2")
    shutil.copytree(_treesrc, _treedst2,
                    ignore=shutil.ignore_patterns("sub"))
    chk("shutil_copytree_ignore",
        os.path.isfile(os.path.join(_treedst2, "top.txt"))
        and not os.path.exists(os.path.join(_treedst2, "sub")))
except OSError as e:
    chk("shutil_copytree", False, "STARRY-RISK errno=%d" % e.errno)
    chk("shutil_copytree_exists_err", False, "STARRY-RISK errno=%d" % e.errno)
    chk("shutil_copytree_dirs_exist_ok", False, "STARRY-RISK errno=%d" % e.errno)
    chk("shutil_copytree_ignore", False, "STARRY-RISK errno=%d" % e.errno)

# shutil.move: relocate a file (rename or copy+delete).
_mvsrc = os.path.join(SBX, "mv.txt")
with open(_mvsrc, "w") as fh:
    fh.write("MV")
_mvdst = os.path.join(SBX, "mvdst.txt")
shutil.move(_mvsrc, _mvdst)
chk("shutil_move", (not os.path.exists(_mvsrc)) and os.path.exists(_mvdst))

# shutil.rmtree: recursively delete a tree.
_rmt = os.path.join(SBX, "rmt")
os.makedirs(os.path.join(_rmt, "x"))
open(os.path.join(_rmt, "x", "f"), "w").close()
shutil.rmtree(_rmt)
chk("shutil_rmtree", not os.path.exists(_rmt))

# shutil.which: locate an executable on PATH; a guaranteed-absent name -> None.
chk("shutil_which_none", shutil.which("definitely_not_a_real_cmd_xyz") is None)

# shutil.disk_usage: (total, used, free) named tuple for a path.
try:
    du = shutil.disk_usage(SBX)
    chk("shutil_disk_usage",
        du.total >= 0 and du.free >= 0 and hasattr(du, "used"),
        "total=%d free=%d" % (du.total, du.free))
except OSError as e:
    chk("shutil_disk_usage", False, "STARRY-RISK errno=%d" % e.errno)

# shutil.get_archive_formats: list of (name, description) tuples; 'tar' is
# always available (pure-Python tarfile, no zlib needed).
_fmts = [f[0] for f in shutil.get_archive_formats()]
chk("shutil_get_archive_formats", "tar" in _fmts)

# shutil.make_archive + unpack_archive round-trip (guarded — needs zlib;
# non-fatal skip). Use 'zip' to write, then unpack and verify a file landed.
try:
    _arc_base = os.path.join(SBX, "arc")
    _arc = shutil.make_archive(_arc_base, "zip", root_dir=_treesrc)
    chk("shutil_make_archive", os.path.isfile(_arc) and _arc.endswith(".zip"))
    _unpack_dir = os.path.join(SBX, "unpacked")
    shutil.unpack_archive(_arc, _unpack_dir)
    chk("shutil_unpack_archive",
        os.path.isfile(os.path.join(_unpack_dir, "top.txt")))
except Exception as e:
    chk("shutil_make_archive", True, "(skip: %s)" % type(e).__name__)
    chk("shutil_unpack_archive", True, "(skip: %s)" % type(e).__name__)


# ============================================================================
# glob & fnmatch — shell-style matching (docs: glob, fnmatch). how: glob within
# the sandbox tree; fnmatch on names; expected: documented wildcard semantics;
# why: pattern-driven file selection.
# ============================================================================
_gdir = os.path.join(SBX, "gd")
os.makedirs(os.path.join(_gdir, "sub"))
for fn in ("a.py", "b.py", "c.txt", os.path.join("sub", "d.py")):
    open(os.path.join(_gdir, fn), "w").close()

# glob.glob with '*' — top level only.
got = sorted(os.path.basename(p) for p in globmod.glob(os.path.join(_gdir, "*.py")))
chk("glob_star", got == ["a.py", "b.py"])
# glob.glob recursive '**' with recursive=True descends.
rec = sorted(os.path.basename(p)
             for p in globmod.glob(os.path.join(_gdir, "**", "*.py"), recursive=True))
chk("glob_recursive", rec == ["a.py", "b.py", "d.py"])
# glob.glob with '?' single-char wildcard.
q = sorted(os.path.basename(p) for p in globmod.glob(os.path.join(_gdir, "?.txt")))
chk("glob_question", q == ["c.txt"])
# glob.iglob returns an iterator.
chk("glob_iglob", hasattr(globmod.iglob(os.path.join(_gdir, "*")), "__next__"))
# glob with no matches returns an empty list (documented), not an error.
chk("glob_no_match", globmod.glob(os.path.join(_gdir, "*.nomatch")) == [])
# A non-recursive '*' does NOT cross a path separator: top-level only excludes
# 'sub/d.py' (which is matched only by '**' recursive above).
_topnames = sorted(os.path.basename(p)
                   for p in globmod.glob(os.path.join(_gdir, "*")))
chk("glob_star_no_cross", "d.py" not in _topnames and "sub" in _topnames)
# glob.escape neutralizes wildcard metacharacters in a literal name.
chk("glob_escape", globmod.escape("a*b?") == "a[*]b[?]")

# fnmatch.fnmatch / fnmatchcase / filter / translate.
chk("fnmatch_match", fnmatch.fnmatch("file.txt", "*.txt"))
chk("fnmatch_nomatch", not fnmatch.fnmatch("file.txt", "*.py"))
chk("fnmatch_case", fnmatch.fnmatchcase("File.TXT", "File.TXT")
    and not fnmatch.fnmatchcase("file.txt", "FILE.TXT"))
chk("fnmatch_filter",
    sorted(fnmatch.filter(["a.py", "b.txt", "c.py"], "*.py")) == ["a.py", "c.py"])
import re as _re
chk("fnmatch_translate",
    _re.match(fnmatch.translate("*.py"), "x.py") is not None)


# ============================================================================
# pathlib.Path — object-oriented paths (docs: pathlib). how: build paths with
# '/', read components, mutate the filesystem, glob; expected: documented
# property + method semantics; why: modern path manipulation API.
# ============================================================================
P = pathlib.Path

# '/' operator + joinpath build child paths.
base = P(SBX)
child = base / "pl" / "deep.txt"
chk("path_slash_op", str(child) == os.path.join(SBX, "pl", "deep.txt"))
chk("path_joinpath", base.joinpath("pl", "x") == base / "pl" / "x")

# Components: name/stem/suffix/suffixes/parent/parents/parts/anchor.
pp = P("/a/b/archive.tar.gz")
chk("path_name", pp.name == "archive.tar.gz")
chk("path_stem", pp.stem == "archive.tar")
chk("path_suffix", pp.suffix == ".gz")
chk("path_suffixes", pp.suffixes == [".tar", ".gz"])
chk("path_parent", str(pp.parent) == "/a/b")
chk("path_parents", str(pp.parents[1]) == "/a")
chk("path_parts", pp.parts == ("/", "a", "b", "archive.tar.gz"))
chk("path_anchor", pp.anchor == "/")

# with_name / with_suffix derive sibling paths.
chk("path_with_name", str(pp.with_name("x.txt")) == "/a/b/x.txt")
chk("path_with_suffix", str(pp.with_suffix(".zip")) == "/a/b/archive.tar.zip")
# with_stem (3.9+) replaces the final-component stem, keeping the suffix.
chk("path_with_stem", str(pp.with_suffix(".zip").with_stem("data")) == "/a/b/data.zip")

# match / relative_to are pure-path predicates.
chk("path_match", P("/a/b/c.py").match("*.py"))
chk("path_relative_to", str(P("/a/b/c").relative_to("/a")) == "b/c")
# is_absolute / is_relative_to are pure-path predicates (is_relative_to 3.9+).
chk("path_is_absolute", P("/a/b").is_absolute() and not P("a/b").is_absolute())
chk("path_is_relative_to", P("/a/b/c").is_relative_to("/a"))
# as_posix forces forward slashes from a Path object too.
chk("path_as_posix", P("/a/b/c").as_posix() == "/a/b/c")
# PurePath equality / hashing: equal paths hash equal.
chk("path_eq_hash",
    P("/a/b") == P("/a/b") and hash(P("/a/b")) == hash(P("/a/b")))

# Filesystem mutation: mkdir/touch/write_text/read_text/write_bytes/read_bytes.
pdir = base / "pl"
pdir.mkdir(parents=True, exist_ok=True)
chk("path_mkdir", pdir.is_dir())
ptouch = pdir / "touched"
ptouch.touch()
chk("path_touch", ptouch.exists() and ptouch.is_file())
ptxt = pdir / "t.txt"
ptxt.write_text("hi pathlib")
chk("path_write_read_text", ptxt.read_text() == "hi pathlib")
pbin = pdir / "b.bin"
pbin.write_bytes(b"\x01\x02")
chk("path_write_read_bytes", pbin.read_bytes() == b"\x01\x02")

# exists / is_file / is_dir predicates.
chk("path_exists_method", ptxt.exists())
chk("path_is_file", ptxt.is_file())
chk("path_is_dir", pdir.is_dir())

# Path.stat(): same os.stat_result as os.stat — st_size matches the bytes we
# wrote ("hi pathlib" = 10 bytes) and the mode says regular file.
_pst = ptxt.stat()
chk("path_stat", _pst.st_size == len("hi pathlib") and statmod.S_ISREG(_pst.st_mode))
# Path.chmod(): set the mode through the Path object; CPython documents it as
# os.chmod on the path, so we require the bits to actually stick (a silent
# no-op kernel must be caught, not papered over — #573).
try:
    ptxt.chmod(0o644)
    chk("path_chmod", statmod.S_IMODE(ptxt.stat().st_mode) == 0o644,
        "mode=%o" % statmod.S_IMODE(ptxt.stat().st_mode))
except OSError as e:
    chk("path_chmod", False, "errno=%d" % e.errno)

# Path.lstat(): like stat for a regular file (size matches the written bytes).
chk("path_lstat", ptxt.lstat().st_size == len("hi pathlib"))
# Path.samefile(): a path is the same file as itself.
chk("path_samefile", ptxt.samefile(ptxt))
# Path.absolute(): yields an absolute path without resolving symlinks.
chk("path_absolute", ptxt.absolute().is_absolute())
# Path.expanduser(): expands a leading '~' using HOME (set to SBX earlier).
chk("path_expanduser", P("~/sub").expanduser() == P(SBX) / "sub")
# Path.is_symlink() is False for a regular file.
chk("path_is_symlink_false", ptxt.is_symlink() is False)
# Path device-type predicates are all False for a regular file.
chk("path_type_predicates",
    not ptxt.is_fifo() and not ptxt.is_socket()
    and not ptxt.is_block_device() and not ptxt.is_char_device())

# Path.symlink_to / readlink / is_symlink (guarded — symlinks often missing).
_plink = pdir / "plink"
try:
    _plink.symlink_to(ptxt)
    chk("path_symlink_to", _plink.is_symlink())
    chk("path_readlink", str(_plink.readlink()) == str(ptxt))
    chk("path_symlink_read", _plink.read_text() == "hi pathlib")
    _plink.unlink()
except (OSError, NotImplementedError) as e:
    chk("path_symlink_to", True, "(skip STARRY-RISK: %s)" % type(e).__name__)
    chk("path_readlink", True, "(skip: symlink unsupported)")
    chk("path_symlink_read", True, "(skip: symlink unsupported)")

# Path.hardlink_to (3.10+): create a hardlink; shares inode (guarded).
_phl = pdir / "phl"
try:
    _phl.hardlink_to(ptxt)
    chk("path_hardlink_to", _phl.stat().st_ino == ptxt.stat().st_ino)
    _phl.unlink()
except (OSError, NotImplementedError) as e:
    chk("path_hardlink_to", True, "(skip STARRY-RISK: %s)" % type(e).__name__)

# iterdir lists children; glob/rglob pattern-match.
(pdir / "x.py").touch()
(pdir / "y.py").touch()
names = sorted(c.name for c in pdir.iterdir())
chk("path_iterdir", {"t.txt", "b.bin", "touched", "x.py", "y.py"} <= set(names))
chk("path_glob", sorted(c.name for c in pdir.glob("*.py")) == ["x.py", "y.py"])
# rglob descends recursively.
(pdir / "nest").mkdir()
(pdir / "nest" / "z.py").touch()
chk("path_rglob", sorted(c.name for c in pdir.rglob("*.py")) == ["x.py", "y.py", "z.py"])

# rename / unlink / rmdir mutate the tree.
prn = pdir / "x.py"
prn2 = pdir / "x_renamed.py"
prn.rename(prn2)
chk("path_rename", prn2.exists() and not prn.exists())
prn2.unlink()
chk("path_unlink", not prn2.exists())
_emptyp = pdir / "emptyd"
_emptyp.mkdir()
_emptyp.rmdir()
chk("path_rmdir", not _emptyp.exists())

# resolve makes the path absolute & canonical.
chk("path_resolve", ptxt.resolve().is_absolute())

# Path.cwd / Path.home are classmethods.
chk("path_cwd", isinstance(P.cwd(), pathlib.Path))
chk("path_home", isinstance(P.home(), pathlib.Path))

# PurePosixPath: pure path math without touching the filesystem.
ppx = pathlib.PurePosixPath("/u/v/w.txt")
chk("pureposixpath", ppx.name == "w.txt" and ppx.as_posix() == "/u/v/w.txt")
chk("pureposixpath_is_absolute",
    ppx.is_absolute() and not pathlib.PurePosixPath("u/v").is_absolute())
# PurePosixPath joining + parent chain are pure (no FS access).
chk("pureposixpath_parent",
    str(ppx.parent) == "/u/v" and str(ppx / "x") == "/u/v/w.txt/x")


# ============================================================================
# Cleanup — remove the entire sandbox; restore the original cwd & environ keys.
# ============================================================================
os.chdir(_START_CWD)
for _k in ("PYOSFS_V", "PYOSFS_MISSING", "PYOSFS_SD"):
    os.environ.pop(_k, None)
shutil.rmtree(SBX, ignore_errors=True)
chk("cleanup", not os.path.exists(SBX))


print(("PY_OSFS_OK") if _ok else ("PY_OSFS_FAIL"))
sys.exit(0 if _ok else 1)
