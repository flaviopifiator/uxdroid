"""What `systemd-socket-activate -l <path> --fdname=<name> <command>` does, for images without it:
binds a listening Unix socket at <path> with the mode the umask leaves, then becomes <command> with
the socket as descriptor 3 and LISTEN_PID (this process, which the command keeps), LISTEN_FDS=1 and
LISTEN_FDNAMES=<name>.

Usage: socket_activate.py <path> <name> <command> [<argument>...]
"""

import os
import socket
import sys

path, name, *command = sys.argv[1:]
listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
listener.bind(path)
listener.listen(16)
if listener.fileno() == 3:
    os.set_inheritable(3, True)
else:
    os.dup2(listener.fileno(), 3, inheritable=True)
    listener.close()
os.environ.update(LISTEN_PID=str(os.getpid()), LISTEN_FDS="1", LISTEN_FDNAMES=name)
os.execv(command[0], command)
