#!/usr/bin/env python3
"""Verify two exact host processes appear and then exit during Flatpak QA."""

from __future__ import annotations

import os
import select
import sys
import time
from pathlib import Path


def process_arguments(pid: int) -> list[bytes] | None:
    try:
        data = Path(f"/proc/{pid}/cmdline").read_bytes()
    except (FileNotFoundError, PermissionError, ProcessLookupError):
        return None
    return [argument for argument in data.split(b"\0") if argument]


def process_identity(pid: int) -> tuple[int, int, int, int, int] | None:
    try:
        data = Path(f"/proc/{pid}/stat").read_bytes()
        fields = data.rsplit(b") ", 1)[1].split()
        return (
            int(fields[2]),
            int(fields[3]),
            int(fields[4]),
            int(fields[5]),
            int(fields[19]),
        )
    except (FileNotFoundError, IndexError, PermissionError, ProcessLookupError, ValueError):
        return None


def exact_processes(arguments: list[bytes]) -> list[int]:
    matches = []
    for entry in Path("/proc").iterdir():
        if entry.name.isdecimal():
            pid = int(entry.name)
            if process_arguments(pid) == arguments:
                matches.append(pid)
    return sorted(matches)


def wait_for_exact(
    arguments: list[bytes], deadline: float
) -> tuple[int, int, int, int, int, int, int]:
    while time.monotonic() < deadline:
        matches = exact_processes(arguments)
        if len(matches) > 1:
            raise RuntimeError(f"multiple processes have exact argv {arguments!r}: {matches}")
        if matches:
            pid = matches[0]
            try:
                pidfd = os.pidfd_open(pid)
            except ProcessLookupError:
                continue
            identity = process_identity(pid)
            if process_arguments(pid) == arguments and identity is not None:
                process_group, session, tty_number, terminal_group, start_time = identity
                return (
                    pid,
                    pidfd,
                    process_group,
                    session,
                    tty_number,
                    terminal_group,
                    start_time,
                )
            os.close(pidfd)
        time.sleep(0.025)
    raise TimeoutError(f"process did not appear with exact argv {arguments!r}")


def main() -> int:
    if len(sys.argv) != 6:
        print(
            "usage: check-flatpak-process-lifecycle.py BG_NAME BG_ARG FG_NAME FG_ARG ACK",
            file=sys.stderr,
        )
        return 2
    background_argv = [os.fsencode(sys.argv[1]), os.fsencode(sys.argv[2])]
    foreground_argv = [os.fsencode(sys.argv[3]), os.fsencode(sys.argv[4])]
    acknowledgement = Path(sys.argv[5])
    deadline = time.monotonic() + 12.0
    opened: list[int] = []
    try:
        background = wait_for_exact(background_argv, deadline)
        opened.append(background[1])
        foreground = wait_for_exact(foreground_argv, deadline)
        opened.append(foreground[1])
        if background[0] == foreground[0]:
            raise RuntimeError("foreground and background resolved to the same PID")
        if background[3] != foreground[3]:
            raise RuntimeError("foreground and background are not in the same session")
        if background[2] == foreground[2]:
            raise RuntimeError("foreground and background are not separate process groups")
        if background[4] <= 0 or background[4] != foreground[4]:
            raise RuntimeError("foreground and background do not share a controlling terminal")
        if foreground[5] != foreground[2]:
            raise RuntimeError("the foreground marker is not the terminal foreground group")
        if background[5] != foreground[2] or background[2] == background[5]:
            raise RuntimeError("the background marker is not a background process group")

        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        descriptor = os.open(acknowledgement, flags, 0o600)
        with os.fdopen(descriptor, "w", encoding="ascii") as stream:
            stream.write(
                f"background={background[0]}:{background[6]} "
                f"foreground={foreground[0]}:{foreground[6]}\n"
            )

        poller = select.poll()
        for pidfd in opened:
            poller.register(pidfd, select.POLLIN)
        remaining = set(opened)
        exit_deadline = time.monotonic() + 12.0
        while remaining and time.monotonic() < exit_deadline:
            timeout_ms = max(1, int((exit_deadline - time.monotonic()) * 1000))
            for pidfd, events in poller.poll(timeout_ms):
                if pidfd not in remaining:
                    continue
                if events & (select.POLLERR | select.POLLNVAL):
                    raise RuntimeError(
                        f"pidfd {pidfd} reported an error event: {events:#x}"
                    )
                if not events & select.POLLIN:
                    raise RuntimeError(
                        f"pidfd {pidfd} reported no exit event: {events:#x}"
                    )
                poller.unregister(pidfd)
                remaining.remove(pidfd)
        if remaining:
            raise TimeoutError("marked host jobs remained after the Flatpak tab closed")
        if exact_processes(background_argv) or exact_processes(foreground_argv):
            raise RuntimeError("a marked host process remained after both pidfds became readable")
    finally:
        for pidfd in opened:
            os.close(pidfd)
    print("Flatpak host foreground and background jobs started and exited")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, TimeoutError) as error:
        print(f"Flatpak host process lifecycle check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
