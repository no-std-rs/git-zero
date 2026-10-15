"""Linux ownership of the shell's detached Apache daemon and CGI descendants."""
import contextlib
import ctypes
import os
from pathlib import Path
import signal
import time


PR_SET_CHILD_SUBREAPER = 36
PR_GET_CHILD_SUBREAPER = 37


def children():
    parent = os.getpid()
    # This dedicated runner never creates threads. Fail closed if embedded in
    # a threaded host, whose other tasks could own children outside this list.
    try:
        with os.scandir("/proc/self/task") as tasks:
            if any(task.name != str(parent) for task in tasks):
                raise RuntimeError("Apache ownership requires a single-threaded runner")
        return list(map(int, Path(f"/proc/{parent}/task/{parent}/children").read_text().split()))
    except (FileNotFoundError, ProcessLookupError) as error:
        # This process is live while executing this read. Missing kernel support
        # cannot be mistaken for an empty owned child set.
        raise RuntimeError("live process has no readable proc children interface") from error


@contextlib.contextmanager
def pid_handle(pid):
    fd = os.pidfd_open(pid)
    try:
        yield fd
    finally:
        os.close(fd)


def signal_child(pid, sig):
    with contextlib.suppress(ChildProcessError, ProcessLookupError), pid_handle(pid) as fd:
        # A reused PID can name a foreign process. Kernel child ownership of
        # this exact pidfd is stronger than a /proc PID/start-time snapshot.
        os.waitid(os.P_PIDFD, fd, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        signal.pidfd_send_signal(fd, sig)


def reap():
    with contextlib.suppress(ChildProcessError):
        while os.waitpid(-1, os.WNOHANG)[0]:
            pass


def settle_children(sig):
    deadline = time.monotonic() + 3
    reap()
    while time.monotonic() < deadline and (owned := children()):
        for pid in owned:
            signal_child(pid, sig)
        time.sleep(0.02)
        reap()
    reap()
    return not children()


def cleanup():
    # Killing an owned parent adopts its remaining descendants as direct
    # children; subsequent sweeps signal/reap them under the same deadline.
    if not settle_children(signal.SIGTERM) and not settle_children(signal.SIGKILL):
        raise RuntimeError("owned Apache/CGI processes survived cleanup deadline")


def require_ownership():
    if children():
        raise RuntimeError("run the adapter without unrelated child processes")
    with pid_handle(os.getpid()) as fd:
        signal.pidfd_send_signal(fd, 0)
        try:
            os.waitid(os.P_PIDFD, fd, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        except ChildProcessError:
            return  # self is not a child; also proves P_PIDFD support pre-launch.
        raise RuntimeError("pidfd unexpectedly names a waitable child")


def adoption_control(operation, value):
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(operation, value, 0, 0, 0):
        raise OSError(ctypes.get_errno(), "cannot change child adoption state")


def adoption_state():
    previous = ctypes.c_int()
    adoption_control(PR_GET_CHILD_SUBREAPER, ctypes.byref(previous))
    return previous.value


@contextlib.contextmanager
def subreaper():
    # Apache daemonizes outside the test shell's process group. Adoption keeps
    # its workers/CGI children owned and waitable even when the shell is killed.
    require_ownership()
    previous = adoption_state()
    with contextlib.ExitStack() as scope:
        adoption_control(PR_SET_CHILD_SUBREAPER, 1)
        scope.callback(adoption_control, PR_SET_CHILD_SUBREAPER, previous)
        scope.callback(cleanup)
        yield
