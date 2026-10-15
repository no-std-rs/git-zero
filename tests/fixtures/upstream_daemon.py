"""Detached daemon/TERM-resistant CGI fixture for the independent harness tests."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def cgi():
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    print("ready", flush=True)
    sys.stdout.close()
    time.sleep(60)


def daemon(pid_file):
    child = subprocess.Popen([sys.executable, __file__, "cgi"], stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    with child.stdout:
        assert child.stdout.readline() == b"ready\n"
    pid_file = Path(pid_file)
    pending = pid_file.with_suffix(".pending")
    pending.write_text(json.dumps([os.getpid(), child.pid]))
    pending.replace(pid_file)
    time.sleep(60)


def shell(pid_file, tap_file, timeout):
    subprocess.Popen([sys.executable, __file__, "daemon", pid_file], start_new_session=True,
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    while not Path(pid_file).exists():
        time.sleep(0.01)
    print(Path(tap_file).read_text(), flush=True)
    if timeout == "1":
        time.sleep(60)


if __name__ == "__main__":
    role, *arguments = sys.argv[1:]
    {"cgi": cgi, "daemon": daemon, "shell": shell}[role](*arguments)
