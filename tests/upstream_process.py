"""Linux ownership of the shell's detached Apache daemon and CGI descendants."""
import contextlib
import ctypes
import os
from pathlib import Path
import signal
import time


def process(pid):
    try:
        text = Path(f"/proc/{pid}/stat").read_text()
    except (FileNotFoundError, ProcessLookupError):
        return None
    fields = text[text.rindex(")") + 2:].split()
    return {"state": fields[0], "parent": int(fields[1]), "start": fields[19]}


def descendants():
    owned = {}
    pending = [os.getpid()]
    while pending:
        parent = pending.pop()
        try:
            children = Path(f"/proc/{parent}/task/{parent}/children").read_text().split()
        except (FileNotFoundError, ProcessLookupError):
            current = process(parent)
            if current is not None and current["state"] not in ("Z", "X"):
                raise RuntimeError("live process has no readable proc children interface")
            continue
        for value in children:
            pid = int(value)
            info = process(pid)
            if info is not None and info["parent"] in (parent, os.getpid()) and pid not in owned:
                owned[pid] = info
                pending.append(pid)
    return owned


def signal_owned(owned, sig):
    for pid, identity in reversed(list(owned.items())):
        try:
            fd = os.pidfd_open(pid)
        except ProcessLookupError:
            continue
        try:
            current = process(pid)
            if current is not None and current["start"] == identity["start"]:
                signal.pidfd_send_signal(fd, sig)
        except ProcessLookupError:
            pass
        finally:
            os.close(fd)


def reap():
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid == 0:
            return


def cleanup():
    for sig in (signal.SIGTERM, signal.SIGKILL):
        deadline = time.monotonic() + 3
        while True:
            reap()
            owned = descendants()
            if not owned:
                return
            signal_owned(owned, sig)
            if time.monotonic() >= deadline:
                break
            time.sleep(0.02)
    raise RuntimeError("owned Apache/CGI processes survived cleanup deadline")


@contextlib.contextmanager
def subreaper():
    # Apache daemonizes outside the test shell's process group. Adoption keeps
    # its workers/CGI children owned and waitable even when the shell is killed.
    if descendants():
        raise RuntimeError("run the adapter without unrelated child processes")
    fd = os.pidfd_open(os.getpid())
    try:
        signal.pidfd_send_signal(fd, 0)
    finally:
        os.close(fd)
    libc = ctypes.CDLL(None, use_errno=True)
    previous = ctypes.c_int()
    if libc.prctl(37, ctypes.byref(previous), 0, 0, 0) or libc.prctl(36, 1, 0, 0, 0):
        raise OSError(ctypes.get_errno(), "cannot own detached Apache children")
    try:
        yield
    finally:
        try:
            cleanup()
        finally:
            if libc.prctl(36, previous.value, 0, 0, 0):
                raise OSError(ctypes.get_errno(), "cannot restore subreaper state")
