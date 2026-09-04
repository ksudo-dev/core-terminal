#!/usr/bin/env python3
"""Build and exercise the Flatpak host supervisor outside a sandbox."""

from __future__ import annotations

import errno
import fcntl
import os
import pty
import resource
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import termios
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
    """Spawn a session leader with an open, but deliberately unclaimed, PTY."""
    master, slave = pty.openpty()
    pid = os.fork()
    if pid == 0:
        try:
            os.close(master)
            os.setsid()
            for descriptor in (0, 1, 2):
                os.dup2(slave, descriptor)
            if slave > 2:
                os.close(slave)
            if nofile_limit is not None:
                resource.setrlimit(
                    resource.RLIMIT_NOFILE, (nofile_limit, nofile_limit)
                )
            os.execv(arguments[0], arguments)
        except BaseException:
            os._exit(127)
    os.close(slave)
    return pid, os.pidfd_open(pid), master


def spawn_in_owned_pty(arguments: list[str]) -> tuple[int, int, int]:
    """Spawn with a PTY already owned by the new process's own session."""
    pid, master = pty.fork()
    if pid == 0:
        try:
            os.execv(arguments[0], arguments)
        except BaseException:
            os._exit(127)
    return pid, os.pidfd_open(pid), master


def spawn_behind_portal_preclaimed_pty(
    arguments: list[str],
) -> tuple[int, int, int]:
    """Model a Flatpak proxy that owns the PTY before a host session starts.

    The launcher remains the controlling terminal's session leader while its
    child starts a separate session and executes the host helper with the same
    slave descriptor. This is the topology produced when VTE does not use
    VTE_PTY_NO_CTTY for the sandbox-side flatpak-spawn process.
    """
    master, slave = pty.openpty()
    launcher = os.fork()
    if launcher == 0:
        try:
            os.close(master)
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            child = os.fork()
            if child == 0:
                try:
                    os.setsid()
                    for descriptor in (0, 1, 2):
                        os.dup2(slave, descriptor)
                    if slave > 2:
                        os.close(slave)
                    os.execv(arguments[0], arguments)
                except BaseException:
                    os._exit(127)
            waited, status = os.waitpid(child, 0)
            if waited != child:
                os._exit(126)
            if os.WIFEXITED(status):
                os._exit(os.WEXITSTATUS(status))
            if os.WIFSIGNALED(status):
                signal.signal(os.WTERMSIG(status), signal.SIG_DFL)
                os.kill(os.getpid(), os.WTERMSIG(status))
            os._exit(126)
        except BaseException:
            os._exit(127)
    os.close(slave)
    return launcher, os.pidfd_open(launcher), master


def read_pty_output(master: int, timeout: float = 0.5) -> bytes:
    output = bytearray()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        remaining = max(0.0, deadline - time.monotonic())
        ready, _, _ = select.select([master], [], [], remaining)
        if not ready:
            break
        try:
            data = os.read(master, 4096)
        except OSError as error:
            if error.errno == errno.EIO:
                break
            raise
        if not data:
            break
        output.extend(data)
    return bytes(output)


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
        acknowledgement_text = acknowledgement.read_text(encoding="ascii")
        if not acknowledgement_text.startswith("topology=PASS "):
            raise RuntimeError(
                f"host lifecycle watcher did not confirm job control: "
                f"{acknowledgement_text.rstrip()}"
            )
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


def already_owned_terminal_test(helper: Path) -> None:
    supervisor_pid = supervisor_pidfd = master = None
    try:
        supervisor_pid, supervisor_pidfd, master = spawn_in_owned_pty(
            [str(helper), "/bin/sh", "-c", "exit 23"]
        )
        status = wait_pid(supervisor_pid)
        if not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 23:
            raise RuntimeError(
                f"supervisor did not accept its existing controlling terminal: {status}"
            )
    finally:
        if supervisor_pidfd is not None:
            try:
                signal.pidfd_send_signal(supervisor_pidfd, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.close(supervisor_pidfd)
        if master is not None:
            os.close(master)


def portal_preclaimed_terminal_rejection_test(helper: Path, suffix: str) -> None:
    marker = f"core-terminal-helper-preclaimed-{suffix}"
    arguments = [os.fsencode(marker), b"44.234"]
    supervisor_arguments = [
        str(helper),
        "/bin/bash",
        "--noprofile",
        "--norc",
        "-c",
        'exec -a "$1" sleep 44.234',
        "core-terminal-helper-test",
        marker,
    ]
    launcher_pid = launcher_pidfd = master = None
    try:
        launcher_pid, launcher_pidfd, master = spawn_behind_portal_preclaimed_pty(
            supervisor_arguments
        )
        status = wait_pid(launcher_pid)
        output = read_pty_output(master)
        if not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 125:
            raise RuntimeError(
                f"supervisor did not reject a portal-preclaimed terminal: {status}; "
                f"output={output!r}"
            )
        if b"core-terminal-host-supervisor: controlling terminal" not in output:
            raise RuntimeError(
                f"preclaimed-terminal rejection lacked its diagnostic: {output!r}"
            )
        if exact_processes(arguments):
            raise RuntimeError("preclaimed-terminal rejection launched the payload")
    finally:
        if launcher_pidfd is not None:
            try:
                signal.pidfd_send_signal(launcher_pidfd, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.close(launcher_pidfd)
        if master is not None:
            os.close(master)
        terminate_exact([os.fsencode(argument) for argument in supervisor_arguments])
        terminate_exact(arguments)


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
        already_owned_terminal_test(helper)
        portal_preclaimed_terminal_rejection_test(helper, suffix)
        normal_exit_cleanup_test(helper, suffix)
        low_descriptor_limit_test(helper, suffix)
    print(
        "Flatpak host supervisor PTY ownership, job control, signals, "
        "and normal-exit cleanup passed"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.SubprocessError, TimeoutError) as error:
        print(f"Flatpak host supervisor check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
