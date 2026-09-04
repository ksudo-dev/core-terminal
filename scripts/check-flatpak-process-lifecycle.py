#!/usr/bin/env python3
"""Verify two exact host processes appear and then exit during Flatpak QA."""

from __future__ import annotations

import os
import select
import signal
import sys
import time
from pathlib import Path
from typing import NamedTuple


DISCOVERY_TIMEOUT_SECONDS = 12.0
TOPOLOGY_TIMEOUT_SECONDS = 2.0
EXIT_TIMEOUT_SECONDS = 12.0


class ProcessSnapshot(NamedTuple):
    pid: int
    pidfd: int
    process_group: int
    session: int
    tty_number: int
    terminal_group: int
    start_time: int


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


def open_exact(arguments: list[bytes]) -> ProcessSnapshot | None:
    matches = exact_processes(arguments)
    if len(matches) > 1:
        raise RuntimeError(f"multiple processes have exact argv {arguments!r}: {matches}")
    if not matches:
        return None
    pid = matches[0]
    identity_before = process_identity(pid)
    if process_arguments(pid) != arguments or identity_before is None:
        return None
    try:
        pidfd = os.pidfd_open(pid)
    except ProcessLookupError:
        return None
    keep_open = False
    try:
        signal.pidfd_send_signal(pidfd, 0)
        identity_after = process_identity(pid)
        if process_arguments(pid) != arguments or identity_after is None:
            return None
        if identity_before[4] != identity_after[4]:
            return None
        process_group, session, tty_number, terminal_group, start_time = identity_after
        snapshot = ProcessSnapshot(
            pid,
            pidfd,
            process_group,
            session,
            tty_number,
            terminal_group,
            start_time,
        )
        keep_open = True
        return snapshot
    except ProcessLookupError:
        return None
    finally:
        if not keep_open:
            os.close(pidfd)


def refresh_exact(
    snapshot: ProcessSnapshot, arguments: list[bytes]
) -> ProcessSnapshot | None:
    try:
        signal.pidfd_send_signal(snapshot.pidfd, 0)
    except ProcessLookupError:
        return None
    identity = process_identity(snapshot.pid)
    if process_arguments(snapshot.pid) != arguments or identity is None:
        return None
    process_group, session, tty_number, terminal_group, start_time = identity
    if start_time != snapshot.start_time:
        return None
    return ProcessSnapshot(
        snapshot.pid,
        snapshot.pidfd,
        process_group,
        session,
        tty_number,
        terminal_group,
        start_time,
    )


def wait_for_pair(
    background_arguments: list[bytes],
    foreground_arguments: list[bytes],
    deadline: float,
) -> tuple[ProcessSnapshot, ProcessSnapshot]:
    while time.monotonic() < deadline:
        background = open_exact(background_arguments)
        try:
            foreground = open_exact(foreground_arguments)
        except BaseException:
            if background is not None:
                os.close(background.pidfd)
            raise
        if background is not None and foreground is not None:
            return background, foreground
        if background is not None:
            os.close(background.pidfd)
        if foreground is not None:
            os.close(foreground.pidfd)
        time.sleep(0.025)
    raise TimeoutError("both exact foreground and background processes did not appear")


def topology_error(
    background: ProcessSnapshot, foreground: ProcessSnapshot
) -> str | None:
    if background.pid == foreground.pid:
        return "foreground and background resolved to the same PID"
    if background.session != foreground.session:
        return "foreground and background are not in the same session"
    if background.process_group == foreground.process_group:
        return "foreground and background are not separate process groups"
    if background.tty_number <= 0 or background.tty_number != foreground.tty_number:
        return "foreground and background do not share a positive controlling terminal"
    if foreground.terminal_group != foreground.process_group:
        return "the foreground marker is not the terminal foreground group"
    if (
        background.terminal_group != foreground.process_group
        or background.process_group == background.terminal_group
    ):
        return "the background marker is not a background process group"
    return None


def describe(snapshot: ProcessSnapshot) -> str:
    return (
        f"pid={snapshot.pid} start={snapshot.start_time} "
        f"session={snapshot.session} pgrp={snapshot.process_group} "
        f"tty={snapshot.tty_number} tpgid={snapshot.terminal_group}"
    )


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
    discovery_deadline = time.monotonic() + DISCOVERY_TIMEOUT_SECONDS
    opened: list[int] = []
    try:
        background, foreground = wait_for_pair(
            background_argv, foreground_argv, discovery_deadline
        )
        opened.extend((background.pidfd, foreground.pidfd))
        last_topology_error = "terminal topology was not sampled"
        topology_deadline = time.monotonic() + TOPOLOGY_TIMEOUT_SECONDS
        while True:
            refreshed_background = refresh_exact(background, background_argv)
            refreshed_foreground = refresh_exact(foreground, foreground_argv)
            if refreshed_background is None or refreshed_foreground is None:
                raise RuntimeError("a marked process exited before acknowledgement")
            background = refreshed_background
            foreground = refreshed_foreground
            last_topology_error = topology_error(background, foreground)
            if last_topology_error is None:
                break
            remaining_topology_time = topology_deadline - time.monotonic()
            if remaining_topology_time <= 0:
                break
            time.sleep(min(0.025, remaining_topology_time))

        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        descriptor = os.open(acknowledgement, flags, 0o600)
        with os.fdopen(descriptor, "w", encoding="ascii") as stream:
            stream.write(
                f"topology={'PASS' if last_topology_error is None else 'FAIL'} "
                f"background=({describe(background)}) "
                f"foreground=({describe(foreground)})\n"
            )

        poller = select.poll()
        for pidfd in opened:
            poller.register(pidfd, select.POLLIN)
        remaining = set(opened)
        exit_deadline = time.monotonic() + EXIT_TIMEOUT_SECONDS
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
        if last_topology_error is not None:
            raise RuntimeError(
                f"{last_topology_error}; background [{describe(background)}]; "
                f"foreground [{describe(foreground)}]"
            )
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
