"""Plays a systemd user manager for one socket unit and its service, as `uxdroid service enable` wrote
them, where the containers have no systemd: binds `ListenStream=` with `SocketMode=` in a directory of
`DirectoryMode=`, and on each first connection while no instance runs, starts `ExecStart=` with the
socket as descriptor 3 and `LISTEN_PID`, `LISTEN_FDS` and `LISTEN_FDNAMES` set as systemd sets them,
with the unit's `Environment=`. Like systemd, it keeps listening between starts, so a connection after
the app exits starts it again. Writes one line per event to its log: `listening <path>`,
`started <pid>`, `exited <pid> <status>`.

Usage: socket_unit.py <unit.socket> <unit.service> <log>
"""

import os
import select
import signal
import socket
import sys


def directives(path):
    """Each `Key=value` of a unit file, in order; repeated keys keep every value."""
    found = []
    with open(path, encoding="utf-8") as unit:
        for line in unit:
            line = line.rstrip("\n")
            if line and not line.startswith(("#", "[")) and "=" in line:
                key, value = line.split("=", 1)
                found.append((key, value))
    return found


def first(found, key):
    values = [value for name, value in found if name == key]
    if len(values) != 1:
        sys.exit(f"socket_unit.py: expected one {key}=, found {len(values)}")
    return values[0]


def unescape(word, command):
    """systemd's specifiers and, on a command line, `$$`: the only ones the units use."""
    runtime = os.environ["XDG_RUNTIME_DIR"]
    out, index = [], 0
    while index < len(word):
        pair = word[index:index + 2]
        if pair == "%%":
            out.append("%")
        elif pair == "%t":
            out.append(runtime)
        elif command and pair == "$$":
            out.append("$")
        elif word[index] in "%$":
            sys.exit(f"socket_unit.py: unexpected {pair!r} in {word!r}")
        else:
            out.append(word[index])
            index += 1
            continue
        index += 2
    return "".join(out)


def main():
    socket_file, service_file, log_path = sys.argv[1:]
    sock = directives(socket_file)
    service = directives(service_file)
    path = unescape(first(sock, "ListenStream"), False)
    mode = int(first(sock, "SocketMode"), 8)
    dir_mode = int(first(sock, "DirectoryMode"), 8)
    name = first(sock, "FileDescriptorName")
    command = [unescape(word, True) for word in first(service, "ExecStart").split(" ")]
    environment = dict(os.environ)
    for key, value in service:
        if key == "Environment":
            variable, setting = value.split("=", 1)
            environment[variable] = unescape(setting, False)
    log = open(log_path, "a", buffering=1, encoding="utf-8")

    os.makedirs(os.path.dirname(path), mode=dir_mode, exist_ok=True)
    if os.path.exists(path):
        os.unlink(path)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(path)
    os.chmod(path, mode)
    listener.listen(16)
    log.write(f"listening {path}\n")
    running = []
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))

    while True:
        if not running:
            # A connection waits in the backlog until the started app accepts it.
            select.select([listener], [], [])
            pid = os.fork()
            if pid == 0:
                if listener.fileno() == 3:
                    os.set_inheritable(3, True)
                else:
                    os.dup2(listener.fileno(), 3, inheritable=True)
                environment.update(LISTEN_PID=str(os.getpid()), LISTEN_FDS="1", LISTEN_FDNAMES=name)
                os.closerange(4, 1024)
                os.execve(command[0], command, environment)
            running.append(pid)
            log.write(f"started {pid}\n")
        pid, status = os.waitpid(running.pop(), 0)
        log.write(f"exited {pid} {os.waitstatus_to_exitcode(status)}\n")


if __name__ == "__main__":
    main()
