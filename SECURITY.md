# Security policy

## Supported versions

Version 0.2.0 is the supported release line. Older package files are not
maintained.

## Reporting a vulnerability

Use a private GitHub Security Advisory for this repository. Include the Core
Terminal version, Ubuntu release, architecture, reproduction steps, expected
behavior, observed behavior, and a log or backtrace with credentials and
terminal input removed. Do not publish an unpatched vulnerability in an issue.

The project will acknowledge the report through the private advisory and post a
fix or status update there. No fixed response time is promised.

## Threat boundaries

Core Terminal starts the configured shell as the current user. A shell or
terminal program can read and modify files and processes available to that
user. Core Terminal does not sandbox shell commands, imported profile values,
or programs started by the shell.

JSON settings and profile documents are bounded to 4 MiB. `.terminal` imports
are parsed as plist data; XML entity and unsupported document-type constructs
are rejected. Imported values are mapped to profile fields and are never
executed. The application validates terminal type, locale, colors, dimensions,
and absolute shell paths before using them. Profile JSON does not store
credentials.

The custom command option intentionally runs through the selected login shell.
The direct command path uses GLib's argument parser, which preserves quotes and
backslash escapes without shell expansion, substitution, or redirection.
Invalid direct commands fall back to the configured login shell. Users must
treat both paths as commands with the current user's privileges.

Custom key mappings are decoded into bounded PTY byte sequences. They cannot
start a process by themselves, but the active shell or terminal program can
interpret the bytes as input. Imported mapping entries pass the same decoder
before they are used.

VTE interprets escape sequences emitted by the child process. Opening a URI or
using a terminal program remains a user action and uses the desktop security
boundary. Wayland does not provide a Core Terminal switch equivalent to macOS
Secure Keyboard Entry.

## Private inputs and package contents

`profiles-private/`, supplied screenshots, `.terminal` files, and reference
archives are not runtime inputs. The Debian build copies an explicit allowlist.
`scripts/check-private-data.sh` checks publishable paths, package contents, and
absolute developer home paths in package files. Run it before publication.

The release checklist also runs `cargo audit`, scans publishable files for
credential patterns, checks the packaged ELF dependencies and dynamic section,
and compares the Debian payload against its documented file set. Commands are
listed in [`docs/TESTING.md`](docs/TESTING.md).
