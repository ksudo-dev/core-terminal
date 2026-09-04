#!/usr/bin/env python3
"""Build and exercise the Flatpak host supervisor outside a sandbox."""

from __future__ import annotations

import os
import pty
import resource
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "src" / "flatpak-host-supervisor.c"


def process_arguments(pid: int) -> list[bytes] | None:
    try:
        data = Path(f"/proc/{pid}/cmdline").read_bytes()
    except (FileNotFoundError, PermissionError, ProcessLookupError):
        return None
    return [argument for argument in data.split(b"\0") if argument]


def exact_processes(arguments: list[bytes]) -> list[int]:
    matches = []
    for entry in Path("/proc").iterdir():
        if entry.name.isdecimal():
            pid = int(entry.name)
            if process_arguments(pid) == arguments:
                matches.append(pid)
    return sorted(matches)


def wait_for_exact(arguments: list[bytes], timeout: float = 4.0) -> tuple[int, int]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        matches = exact_processes(arguments)
        if len(matches) > 1:
            raise RuntimeError(f"multiple exact processes found for {arguments!r}: {matches}")
        if matches:
            pid = matches[0]
            try:
                pidfd = os.pidfd_open(pid)
            except ProcessLookupError:
                continue
            if process_arguments(pid) == arguments:
                return pid, pidfd
            os.close(pidfd)
        time.sleep(0.01)
    raise TimeoutError(f"process did not appear with exact argv {arguments!r}")


def wait_pid(pid: int, timeout: float = 6.0) -> int:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        waited, status = os.waitpid(pid, os.WNOHANG)
        if waited == pid:
            return status
        time.sleep(0.01)
    raise TimeoutError(f"process {pid} did not exit")


def wait_pidfd(pidfd: int, timeout: float = 4.0) -> None:
    poller = select.poll()
    poller.register(pidfd, select.POLLIN)
    if not poller.poll(round(timeout * 1000)):
        raise TimeoutError("pidfd did not report process exit")


def wait_for_exact_count(
    arguments: list[bytes], minimum: int, timeout: float = 4.0
) -> list[int]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        matches = exact_processes(arguments)
        if len(matches) >= minimum:
            return matches
        time.sleep(0.01)
    raise TimeoutError(
        f"only {len(exact_processes(arguments))} of {minimum} exact processes appeared"
    )


def spawn_in_pty(
    arguments: list[str], nofile_limit: int | None = None
) -> tuple[int, int, int]:
    pid, master = pty.fork()
    if pid == 0:
        if nofile_limit is not None:
            resource.setrlimit(resource.RLIMIT_NOFILE, (nofile_limit, nofile_limit))
        os.execv(arguments[0], arguments)
    return pid, os.pidfd_open(pid), master


def terminate_exact(arguments: list[bytes]) -> None:
    for pid in exact_processes(arguments):
        try:
            pidfd = os.pidfd_open(pid)
        except ProcessLookupError:
            continue
        try:
            if process_arguments(pid) == arguments:
                signal.pidfd_send_signal(pidfd, signal.SIGKILL)
        finally:
            os.close(pidfd)


def verify_helper_hardening(helper: Path) -> None:
    readelf = shutil.which("readelf")
    if readelf is None:
        raise RuntimeError("readelf is required")
    header = subprocess.run(
        [readelf, "-h", str(helper)], check=True, text=True, capture_output=True
    ).stdout
    dynamic = subprocess.run(
        [readelf, "-d", str(helper)], check=True, text=True, capture_output=True
    ).stdout
    program_headers = subprocess.run(
        [readelf, "-lW", str(helper)], check=True, text=True, capture_output=True
    ).stdout
    symbols = subprocess.run(
        [readelf, "-sW", str(helper)], check=True, text=True, capture_output=True
    ).stdout
    if "DYN (Position-Independent Executable file)" not in header:
        raise RuntimeError("host supervisor is not a position-independent executable")
    if "(NEEDED)" in dynamic:
        raise RuntimeError("host supervisor has a dynamic library dependency")
    stack_rows = [line for line in program_headers.splitlines() if "GNU_STACK" in line]
    if "GNU_RELRO" not in program_headers or len(stack_rows) != 1:
        raise RuntimeError("host supervisor is missing ELF hardening metadata")
    if "E" in stack_rows[0].split()[-2]:
        raise RuntimeError("host supervisor requests an executable stack")
    if "__stack_chk_fail" not in symbols:
        raise RuntimeError("host supervisor was built without stack protection")


def signal_cleanup_test(
    helper: Path, work: Path, suffix: str, signal_number: signal.Signals
) -> None:
    signal_label = signal_number.name.lower().removeprefix("sig")
    background = f"core-terminal-helper-{signal_label}-bg-{suffix}"
    foreground = f"core-terminal-helper-{signal_label}-fg-{suffix}"
    acknowledgement = work / f"{signal_label}-jobs-seen"
    watcher = subprocess.Popen(
        [
            str(ROOT / "scripts" / "check-flatpak-process-lifecycle.py"),
            background,
            "41.234",
            foreground,
            "40.234",
            str(acknowledgement),
        ]
    )
    supervisor_pid = supervisor_pidfd = master = None
    try:
        supervisor_pid, supervisor_pidfd, master = spawn_in_pty(
            [
                str(helper),
                "/bin/bash",
                "--noprofile",
                "--norc",
                "-i",
                "-c",
                "set -m; "
                '(trap "" HUP INT QUIT TERM USR1 USR2; '
                'exec -a "$1" sleep 41.234) & '
                'trap "" HUP INT QUIT TERM USR1 USR2; '
                'exec -a "$2" sleep 40.234',
                "core-terminal-helper-test",
                background,
                foreground,
            ]
        )
        deadline = time.monotonic() + 4.0
        while not acknowledgement.is_file() and time.monotonic() < deadline:
            if watcher.poll() is not None:
                raise RuntimeError("host lifecycle watcher exited before acknowledgement")
            time.sleep(0.01)
        if not acknowledgement.is_file():
            raise TimeoutError("host lifecycle watcher did not acknowledge both jobs")
        signal.pidfd_send_signal(supervisor_pidfd, signal_number)
        watcher_status = watcher.wait(timeout=14)
        if watcher_status != 0:
            raise RuntimeError(f"host lifecycle watcher exited {watcher_status}")
        status = wait_pid(supervisor_pid)
        if not os.WIFSIGNALED(status) or os.WTERMSIG(status) != signal_number:
            raise RuntimeError(
                f"supervisor did not preserve {signal_number.name} status: {status}"
            )
    finally:
        if watcher.poll() is None:
            watcher.terminate()
            watcher.wait(timeout=2)
        if supervisor_pidfd is not None:
            try:
                signal.pidfd_send_signal(supervisor_pidfd, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.close(supervisor_pidfd)
        if master is not None:
            os.close(master)
        terminate_exact([os.fsencode(background), b"41.234"])
        terminate_exact([os.fsencode(foreground), b"40.234"])


def normal_exit_cleanup_test(helper: Path, suffix: str) -> None:
    marker = f"core-terminal-helper-normal-{suffix}"
    arguments = [os.fsencode(marker), b"42.234"]
    supervisor_pid = supervisor_pidfd = master = marker_pidfd = None
    try:
        supervisor_pid, supervisor_pidfd, master = spawn_in_pty(
            [
                str(helper),
                "/bin/bash",
                "--noprofile",
                "--norc",
                "-c",
                'set -m; trap "" HUP TERM; exec -a "$1" sleep 42.234 & exit 7',
                "core-terminal-helper-test",
                marker,
            ]
        )
        _marker_pid, marker_pidfd = wait_for_exact(arguments)
        status = wait_pid(supervisor_pid)
        if not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 7:
            raise RuntimeError(f"supervisor did not preserve exit status 7: {status}")
        wait_pidfd(marker_pidfd)
        if exact_processes(arguments):
            raise RuntimeError("normal child exit left a marked session process")
    finally:
        if marker_pidfd is not None:
            os.close(marker_pidfd)
        if supervisor_pidfd is not None:
            try:
                signal.pidfd_send_signal(supervisor_pidfd, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.close(supervisor_pidfd)
        if master is not None:
            os.close(master)
        terminate_exact(arguments)


def low_descriptor_limit_test(helper: Path, suffix: str) -> None:
    marker = f"core-terminal-helper-nofile-{suffix}"
    arguments = [os.fsencode(marker), b"43.234"]
    supervisor_pid = supervisor_pidfd = master = None
    try:
        supervisor_pid, supervisor_pidfd, master = spawn_in_pty(
            [
                str(helper),
                "/bin/bash",
                "--noprofile",
                "--norc",
                "-c",
                'set -m; trap "" HUP INT QUIT TERM USR1 USR2; '
                "for _ in {1..40}; do "
                '(trap "" HUP INT QUIT TERM USR1 USR2; '
                'exec -a "$1" sleep 43.234) & '
                "done; wait",
                "core-terminal-helper-test",
                marker,
            ],
            nofile_limit=8,
        )
        wait_for_exact_count(arguments, 32)
        signal.pidfd_send_signal(supervisor_pidfd, signal.SIGHUP)
        status = wait_pid(supervisor_pid, timeout=8)
        if not os.WIFSIGNALED(status) or os.WTERMSIG(status) != signal.SIGHUP:
            raise RuntimeError(
                f"low-descriptor supervisor did not preserve SIGHUP status: {status}"
            )
        deadline = time.monotonic() + 4.0
        while exact_processes(arguments) and time.monotonic() < deadline:
            time.sleep(0.01)
        if exact_processes(arguments):
            raise RuntimeError("low-descriptor cleanup left marked session processes")
    finally:
        if supervisor_pidfd is not None:
            try:
                signal.pidfd_send_signal(supervisor_pidfd, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.close(supervisor_pidfd)
        if master is not None:
            os.close(master)
        terminate_exact(arguments)


def main() -> int:
    compiler = shutil.which("cc")
    if compiler is None:
        print("cc is required", file=sys.stderr)
        return 2
    with tempfile.TemporaryDirectory(prefix="core-terminal-host-supervisor-") as directory:
        work = Path(directory)
        helper = work / "core-terminal-host-supervisor"
        subprocess.run(
            [
                compiler,
                "-std=c11",
                "-O2",
                "-D_FORTIFY_SOURCE=3",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-fstack-protector-strong",
                "-fPIE",
                "-static-pie",
                "-Wl,-z,relro,-z,now",
                str(SOURCE),
                "-o",
                str(helper),
            ],
            check=True,
        )
        verify_helper_hardening(helper)
        suffix = f"{os.getpid()}-{time.monotonic_ns()}"
        for signal_number in (
            signal.SIGHUP,
            signal.SIGINT,
            signal.SIGQUIT,
            signal.SIGTERM,
            signal.SIGUSR1,
            signal.SIGUSR2,
        ):
            signal_cleanup_test(helper, work, suffix, signal_number)
        normal_exit_cleanup_test(helper, suffix)
        low_descriptor_limit_test(helper, suffix)
    print("Flatpak host supervisor signal and normal-exit cleanup passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.SubprocessError, TimeoutError) as error:
        print(f"Flatpak host supervisor check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
