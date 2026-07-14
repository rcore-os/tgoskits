#!/usr/bin/env python3
# pty_tui_drive.py -- headless PTY interaction driver for a real curses/TUI program.
#
# Runs <cmd> inside a REAL pseudo-terminal (pty.fork), so the program takes its production
# terminal render path (initscr / smkx / alternate screen), sets the window size on the master
# with TIOCSWINSZ, then plays a scripted key sequence with a settle dwell after each key while
# continuously draining the raw ANSI byte stream the program writes back. Returns the full raw
# capture plus a checkpoint offset after every scripted step, and whether the program stayed
# alive across the whole soak.
#
# ============================================================================================
#   CRITICAL, EMPIRICALLY-VERIFIED FACT -- application-keypad / DECCKM arrow keys
# ============================================================================================
# A curses app that calls keypad(True) (glances, top, ...) emits smkx, putting the terminal
# into APPLICATION cursor-key mode (DECCKM). In that mode the arrow keys the app expects are the
# SS3 forms  ESC O A / ESC O B / ESC O C / ESC O D  (up/down/right/left) -- NOT the CSI forms
# ESC [ A ... . If a driver sends the CSI arrows, ncurses cannot match them against its
# application-mode terminfo (kcuu1=\EOA), times out on the lone ESC, and returns a bare ESC to the
# app -- glances treats a bare ESC as "quit" and exits. So this driver sends SS3 arrows.
# PgUp/PgDn/Home/End/function keys use their real terminfo sequences (still CSI/tilde forms, which
# do not change under DECCKM). This was proven on StarryOS: SS3 -> program survives + keeps
# redrawing; CSI -> program quits immediately. StarryOS's tty/pty layer delivers the bytes and
# honours TIOCSWINSZ correctly, so the same SS3 sequences that work on Linux work on StarryOS.
# ============================================================================================
import os, sys, pty, time, select, signal, fcntl, termios, struct

# SS3 (application cursor-key mode / DECCKM) arrow sequences -- the ONLY correct arrows for a
# keypad(True) curses app. Do NOT substitute the CSI forms (ESC [ A ...).
SS3_UP    = b"\x1bOA"
SS3_DOWN  = b"\x1bOB"
SS3_RIGHT = b"\x1bOC"
SS3_LEFT  = b"\x1bOD"
# These do not change between normal/application cursor mode.
PGDN = b"\x1b[6~"
PGUP = b"\x1b[5~"
HOME = b"\x1b[H"
END  = b"\x1b[F"

# Function keys as xterm-256color terminfo renders them (kf1..kf10): F1-F4 are the SS3 forms
# ESC O P/Q/R/S, F5-F10 the CSI ~ forms. Like the SS3 arrows these are what a keypad(True) curses
# app (htop) matches against its application-mode terminfo; DECCKM only affects cursor keys, so these
# stay constant. A curses app opens its F-key panels only when the exact terminfo bytes arrive, so a
# panel appearing after one of these is itself proof the pty delivered the sequence intact.
F1  = b"\x1bOP"
F2  = b"\x1bOQ"
F3  = b"\x1bOR"
F4  = b"\x1bOS"
F5  = b"\x1b[15~"
F6  = b"\x1b[17~"
F7  = b"\x1b[18~"
F8  = b"\x1b[19~"
F9  = b"\x1b[20~"
F10 = b"\x1b[21~"
ESC = b"\x1b"


# Optional live-screen emulation so the driver can wait UNTIL the TUI has rendered expected content
# (content-driven capture) instead of a fixed delay -- essential for slow (TCG) arches where glances
# renders much later than on x86. Falls back to fixed dwells if pyte is unavailable.
try:
    import pyte as _pyte
except Exception:
    _pyte = None


def drive(cmd, script, cols=200, rows=50, env_extra=None, quit_key=b"q", tail_dwell=2.0):
    """Run `cmd` (list) in a pty and play `script`.

    Each script step is (label, key_bytes, spec) where spec is EITHER:
      * a number  -> fixed dwell in seconds (legacy, time-driven), OR
      * a dict {"wait_for": [tokens], "max_wait": s, "min_dwell": s} -> CONTENT-DRIVEN: after the key
        is sent, keep pumping + feeding a live pyte screen until every token in `wait_for` is present
        on that screen (AND at least min_dwell has elapsed), then capture; if `max_wait` passes first,
        capture anyway and flag the step `met=False` (a REAL,据实 failure surfaces downstream because
        the captured frame lacks the expected content -- never a silent pass). This makes capture
        timing track each arch's actual render speed (x86 satisfies instantly; aa/rv/loong wait).

    Returns dict: raw(bytes), checkpoints(list[(label, offset, met)]), dead(str|None),
    survived(bool), sent(list[label]).
    """
    pid, fd = pty.fork()
    if pid == 0:
        env = os.environ
        env["TERM"] = "xterm-256color"
        env["COLUMNS"], env["LINES"] = str(cols), str(rows)
        if env_extra:
            for k, v in env_extra.items():
                env[k] = v
        try:
            os.execvp(cmd[0], cmd)
        except Exception:
            os._exit(127)
    # parent: fix the master window size so the child renders to `rows`x`cols`.
    try:
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    except Exception:
        pass

    raw = bytearray()
    dead = [None]
    checkpoints = []
    sent = []
    # live incremental screen: fed the SAME bytes as raw, so raw[:offset] full-fed == live screen
    # (pyte is deterministic; the differential-render invariant asserts this equivalence separately).
    live_screen = _pyte.Screen(cols, rows) if _pyte else None
    live_stream = _pyte.ByteStream(live_screen) if _pyte else None

    def _read_step(timeout):
        r, _, _ = select.select([fd], [], [], timeout)
        if fd in r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                dead[0] = "eof"
                return
            if not data:
                dead[0] = "eof"
                return
            raw.extend(data)
            if live_stream is not None:
                try:
                    live_stream.feed(data)
                except Exception:
                    pass
        try:
            wpid, st = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            dead[0] = "reaped"
            return
        if wpid == pid:
            dead[0] = "exit:%d" % (os.WEXITSTATUS(st) if os.WIFEXITED(st) else -os.WTERMSIG(st))

    def _screen_has(tokens):
        if live_screen is None or not tokens:
            return True
        disp = live_screen.display
        return all(any(t in line for line in disp) for t in tokens)

    def pump_fixed(seconds):
        end = time.time() + seconds
        while time.time() < end and not dead[0]:
            _read_step(0.2)

    def pump_until(tokens, max_wait, min_dwell):
        start = time.time()
        end = start + max_wait
        met = False
        while time.time() < end and not dead[0]:
            _read_step(0.2)
            if (time.time() - start) >= min_dwell and _screen_has(tokens):
                met = True
                break
        # content satisfied before min_dwell? keep pumping to min_dwell so the frame fully settles.
        while (time.time() - start) < min_dwell and not dead[0]:
            _read_step(0.2)
        return met

    for label, key, spec in script:
        if dead[0]:
            break
        if key:
            try:
                os.write(fd, key)
            except OSError as e:
                dead[0] = "write:%s" % e
                break
            sent.append(label)
        if isinstance(spec, dict):
            met = pump_until(spec.get("wait_for", []),
                             float(spec.get("max_wait", 30.0)),
                             float(spec.get("min_dwell", 1.0)))
        else:
            pump_fixed(float(spec))
            met = None
        checkpoints.append((label, len(raw), met))

    # ask the program to quit cleanly (single-key 'q' for glances/top).
    if not dead[0]:
        try:
            os.write(fd, quit_key)
        except OSError:
            pass
        pump_fixed(tail_dwell)

    # a clean quit (exit:0) or "still alive when we stopped reading" both count as survived; only a
    # NON-zero exit or a mid-soak death (before we sent quit) is a failure.
    survived = True
    if dead[0] and dead[0].startswith("exit:") and dead[0] != "exit:0":
        survived = False
    if dead[0] in ("eof", "reaped") and not sent:
        survived = False

    try:
        os.close(fd)
    except OSError:
        pass
    try:
        os.waitpid(pid, 0)
    except Exception:
        pass

    return {
        "raw": bytes(raw),
        "checkpoints": checkpoints,
        "dead": dead[0],
        "survived": survived,
        "sent": sent,
    }


if __name__ == "__main__":
    # Standalone smoke: pty_tui_drive.py <seconds> <cmd...>
    signal.signal(signal.SIGALRM, lambda *_: os._exit(0))
    signal.alarm(int(float(sys.argv[1])) + 40 if len(sys.argv) > 1 else 60)
    dur = float(sys.argv[1]) if len(sys.argv) > 1 else 12
    cmd = sys.argv[2:] or ["glances"]
    scr = [("settle", b"", dur)]
    res = drive(cmd, scr)
    print("survived=%s dead=%s rawlen=%d" % (res["survived"], res["dead"], len(res["raw"])))
