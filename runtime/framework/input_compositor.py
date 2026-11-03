#!/usr/bin/env python3
"""A scripted Wayland compositor with a seat, for input smoke tests and benchmarks.

Weston's headless backend has no way to inject input, so this compositor plays one: it serves
wl_compositor, wl_subcompositor, wl_shm, xdg_wm_base and a wl_seat with a pointer, a keyboard (with
the xkb keymap in a file) and a touchscreen, keeps the last frame of the toplevel with its subsurfaces
composed over it, sends input to the surface under the point, and takes commands from a FIFO, one
per line:

    click X Y           pointer enter, move, left button down and up
    tap X Y             one finger down and up
    drag X1 Y1 X2 Y2    one finger down, three moves, up
    scroll X Y STEPS    wheel steps at X Y, positive is down
    type TEXT           keys of a US keyboard, with shift for capitals and symbols
    key CODE            one evdev key down and up
    unfocus / focus     deactivate or activate the window
    ping                an xdg_wm_base ping, answered or not
    shot PATH           write the window's last frame as a binary PPM
    resize W H          configure the toplevel to W by H
    close               ask the toplevel to close

It prints `[compositor] commit N WxH damage X,Y,W,H` for each frame of the toplevel,
`[compositor] subsurface S commit WxH at X,Y damage X,Y,W,H` for each of a subsurface, `subsurface S
created` and `destroyed`, and after each input command, the time until the first frame whose pixels
differ from the frame shown when the input was sent: `[compositor] changed MS ms after COMMAND`.

usage: input_compositor.py SOCKET FIFO KEYMAP_FILE
"""
import hashlib
import os
import select
import socket
import struct
import sys
import time

path, fifo_path, keymap_path = sys.argv[1], sys.argv[2], sys.argv[3]


def log(*args):
    print("[compositor]", *args, flush=True)


# xkb keymap text, from `uxdroid-keymap us`; the compositor sends it with a NUL, as xkb writes it.
KEYMAP = open(keymap_path, "rb").read().rstrip(b"\0") + b"\0"
# US layout: character -> (evdev code, shift)
US = {}
for row, codes in (("1234567890-=", range(2, 14)), ("qwertyuiop[]", range(16, 28)),
                   ("asdfghjkl;'`", range(30, 42)), ("\\zxcvbnm,./", range(43, 54))):
    for char, code in zip(row, codes):
        US[char] = (code, False)
for row, shifted in (("1234567890-=", "!@#$%^&*()_+"), ("[]", "{}"), (";'`", ':"~'), ("\\,./", "|<>?")):
    for plain, char in zip(row, shifted):
        US[char] = (US[plain][0], True)
for char in "abcdefghijklmnopqrstuvwxyz":
    US[char.upper()] = (US[char][0], True)
US[" "] = (57, False)
US["\n"] = (28, False)
SHIFT = 42

try:
    os.unlink(path)
except OSError:
    pass
server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen(1)
os.mkfifo(fifo_path) if not os.path.exists(fifo_path) else None
fifo = os.open(fifo_path, os.O_RDWR | os.O_NONBLOCK)
connection, _ = server.accept()
start = time.monotonic()

objects = {1: "wl_display"}
pools, buffers, surfaces = {}, {}, {}
incoming_fds, data, commands = [], b"", b""
serial = 1000
xdg_surfaces, toplevels = {}, {}  # xdg_surface -> wl_surface, toplevel -> xdg_surface
window = None  # (toplevel, wl_surface) of the newest window
pointers, keyboards, touches = [], [], []
seat_version = 1
frame = {"count": 0, "pixels": None, "size": (0, 0), "hash": None}
damage = {}  # surface -> [x, y, w, h] since its last commit
contents = {}  # surface -> (pixels, width, height)
subsurfaces = []  # [subsurface, surface, x, y], bottom first
pointer_on = None  # the surface the pointer is in
size_wanted = (0, 0)
waiting = []  # (sent time, command, hash when sent)
focused = False
wm_base = None


def string(text):
    raw = text.encode() + b"\0"
    return struct.pack("<I", len(raw)) + raw + b"\0" * (-len(raw) % 4)


def array(raw):
    return struct.pack("<I", len(raw)) + raw + b"\0" * (-len(raw) % 4)


def send(object_id, opcode, body=b"", fds=()):
    message = struct.pack("<II", object_id, ((8 + len(body)) << 16) | opcode) + body
    if fds:
        connection.sendmsg([message], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, struct.pack(f"<{len(fds)}i", *fds))])
    else:
        connection.sendall(message)


def next_serial():
    global serial
    serial += 1
    return serial


def ms():
    return int((time.monotonic() - start) * 1000) & 0xFFFFFFFF


def fixed(value):
    return struct.pack("<i", int(round(value * 256)))


def configure(toplevel, activated):
    states = struct.pack("<I", 4) if activated else b""
    send(toplevel, 0, struct.pack("<ii", *size_wanted) + array(states))
    send(toplevels[toplevel], 0, struct.pack("<I", next_serial()))


def set_focus(on):
    global focused
    if window is None or focused == on:
        return
    focused = on
    toplevel, surface = window
    configure(toplevel, on)
    for keyboard in keyboards:
        if on:
            send(keyboard, 1, struct.pack("<II", next_serial(), surface) + array(b""))
            send(keyboard, 4, struct.pack("<IIIII", next_serial(), 0, 0, 0, 0))
        else:
            send(keyboard, 2, struct.pack("<II", next_serial(), surface))


def pointer_frame(pointer):
    if seat_version >= 5:
        send(pointer, 5)


def key(code, pressed):
    for keyboard in keyboards:
        send(keyboard, 3, struct.pack("<IIII", next_serial(), ms(), code, 1 if pressed else 0))


def modifiers(shift):
    for keyboard in keyboards:
        send(keyboard, 4, struct.pack("<IIIII", next_serial(), 1 if shift else 0, 0, 0, 0))


def under(x, y):
    """The surface under display point (x, y), and the point in its coordinates."""
    for _, surface, sx, sy in reversed(subsurfaces):
        if surface in contents:
            _, width, height = contents[surface]
            if sx <= x < sx + width and sy <= y < sy + height:
                return surface, x - sx, y - sy
    return window[1], x, y


def enter(pointer, surface, x, y):
    global pointer_on
    if pointer_on is not None and pointer_on != surface:
        send(pointer, 1, struct.pack("<II", next_serial(), pointer_on))
        pointer_frame(pointer)
    pointer_on = surface
    send(pointer, 0, struct.pack("<II", next_serial(), surface) + fixed(x) + fixed(y))
    pointer_frame(pointer)


def run(command):
    words = command.split(" ", 1)
    name, rest = words[0], words[1] if len(words) > 1 else ""
    if window is None:
        log("no window for", command)
        return
    surface = window[1]
    if name == "click":
        surface, x, y = under(*map(float, rest.split()))
        for pointer in pointers:
            enter(pointer, surface, x, y)
            send(pointer, 2, struct.pack("<I", ms()) + fixed(x) + fixed(y))
            pointer_frame(pointer)
            send(pointer, 3, struct.pack("<IIII", next_serial(), ms(), 0x110, 1))
            pointer_frame(pointer)
            send(pointer, 3, struct.pack("<IIII", next_serial(), ms(), 0x110, 0))
            pointer_frame(pointer)
    elif name == "scroll":
        x, y, steps = rest.split()
        for pointer in pointers:
            enter(pointer, surface, float(x), float(y))
            if seat_version >= 5:
                send(pointer, 8, struct.pack("<Ii", 0, int(steps)))
            send(pointer, 4, struct.pack("<II", ms(), 0) + fixed(10.0 * int(steps)))
            pointer_frame(pointer)
    elif name in ("tap", "drag"):
        points = list(map(float, rest.split()))
        surface, x, y = under(points[0], points[1])
        dx, dy = x - points[0], y - points[1]
        points = [p + (dx if i % 2 == 0 else dy) for i, p in enumerate(points)]
        for touch in touches:
            send(touch, 0, struct.pack("<IIIi", next_serial(), ms(), surface, 7) + fixed(x) + fixed(y))
            send(touch, 3)
            if name == "drag":
                x2, y2 = points[2], points[3]
                for step in range(1, 4):
                    mx, my = x + (x2 - x) * step / 3, y + (y2 - y) * step / 3
                    send(touch, 2, struct.pack("<Ii", ms(), 7) + fixed(mx) + fixed(my))
                    send(touch, 3)
            send(touch, 1, struct.pack("<IIi", next_serial(), ms(), 7))
            send(touch, 3)
    elif name == "type":
        text = rest.encode().decode("unicode_escape")
        for char in text:
            code, shift = US[char]
            if shift:
                modifiers(True)
                key(SHIFT, True)
            key(code, True)
            key(code, False)
            if shift:
                key(SHIFT, False)
                modifiers(False)
    elif name == "key":
        key(int(rest), True)
        key(int(rest), False)
    elif name == "focus":
        set_focus(True)
    elif name == "unfocus":
        set_focus(False)
    elif name == "resize":
        global size_wanted
        size_wanted = tuple(map(int, rest.split()))
        configure(window[0], focused)
        log("sent resize", rest)
        return
    elif name == "close":
        send(window[0], 1)
        log("sent close")
        return
    elif name == "ping":
        send(wm_base, 0, struct.pack("<I", 4242))
        log("sent ping 4242")
        return
    elif name == "shot":
        width, height = frame["size"]
        with open(rest, "wb") as out:
            out.write(b"P6 %d %d 255\n" % (width, height))
            pixels = frame["pixels"]
            rgb = bytearray(width * height * 3)
            rgb[0::3], rgb[1::3], rgb[2::3] = pixels[2::4], pixels[1::4], pixels[0::4]
            out.write(rgb)
        log("shot", rest)
        return
    else:
        log("unknown command", command)
        return
    waiting.append((time.monotonic(), command, frame["hash"]))
    log("sent", command)


configured = set()


def compose():
    """The toplevel's last frame with the subsurfaces over it."""
    pixels, width, height = contents[window[1]]
    if not any(surface in contents for _, surface, _, _ in subsurfaces):
        return pixels
    out = bytearray(pixels)
    for _, surface, sx, sy in subsurfaces:
        if surface not in contents:
            continue
        sub, sw, sh = contents[surface]
        for row in range(max(0, sy), min(height, sy + sh)):
            left, right = max(0, sx), min(width, sx + sw)
            if right <= left:
                continue
            start = ((row - sy) * sw + (left - sx)) * 4
            out[(row * width + left) * 4:(row * width + right) * 4] = sub[start:start + (right - left) * 4]
    return bytes(out)


def commit(surface):
    # The first commit of a new toplevel asks for its first configure, as xdg-shell requires.
    for toplevel, xdg in toplevels.items():
        if xdg_surfaces.get(xdg) == surface and toplevel not in configured:
            configured.add(toplevel)
            configure(toplevel, False)
            return
    attached = surfaces.get(surface)
    sub = next((entry for entry in subsurfaces if entry[1] == surface), None)
    if window is None or (window[1] != surface and sub is None):
        return
    if window[1] == surface:
        # A subsurface's position applies with its parent's commit.
        for entry in subsurfaces:
            if len(entry) > 4:
                entry[2], entry[3] = entry.pop()
    if attached in buffers:
        fd, offset, width, height, stride = buffers[attached]
        raw = os.pread(fd, stride * height, offset)
        if stride != width * 4:
            raw = b"".join(raw[row * stride:row * stride + width * 4] for row in range(height))
        contents[surface] = (raw, width, height)
    if surface not in contents or window[1] not in contents:
        return
    _, width, height = contents[surface]
    area = ",".join(map(str, damage.pop(surface, [0, 0, 0, 0])))
    if sub is not None:
        log(f"subsurface {sub[0]} commit {width}x{height} at {sub[2]},{sub[3]} damage {area}")
    raw = compose()
    width, height = contents[window[1]][1:]
    frame["count"] += 1
    frame["pixels"], frame["size"] = raw, (width, height)
    frame["hash"] = hashlib.md5(raw).digest()
    if sub is None:
        log(f"commit {frame['count']} {width}x{height} damage {area}")
    now = time.monotonic()
    for entry in list(waiting):
        sent, command, before = entry
        if frame["hash"] != before:
            waiting.remove(entry)
            log(f"changed {(now - sent) * 1000:.1f} ms after {command}")
    if frame["count"] == 1:
        set_focus(True)


def handle(object_id, opcode, body):
    global window, seat_version, wm_base
    kind = objects.get(object_id, "?")
    word = lambda i: struct.unpack_from("<I", body, i)[0]
    if kind == "wl_display":
        if opcode == 0:
            callback = word(0)
            send(callback, 0, struct.pack("<I", 0))
            send(1, 1, struct.pack("<I", callback))
        elif opcode == 1:
            registry = word(0)
            objects[registry] = "wl_registry"
            for name, interface, version in ((1, "wl_compositor", 4), (2, "wl_shm", 1), (3, "xdg_wm_base", 1),
                                             (4, "wl_seat", 7), (5, "wl_subcompositor", 1)):
                send(registry, 0, struct.pack("<I", name) + string(interface) + struct.pack("<I", version))
    elif kind == "wl_registry":
        length = word(4)
        interface = body[8:8 + length - 1].decode()
        position = 8 + length + (-length % 4)
        version, new = struct.unpack_from("<II", body, position)
        objects[new] = interface
        if interface == "wl_shm":
            send(new, 0, struct.pack("<I", 0))
            send(new, 0, struct.pack("<I", 1))
        elif interface == "xdg_wm_base":
            wm_base = new
        elif interface == "wl_seat":
            seat_version = version
            send(new, 0, struct.pack("<I", 7))
            if version >= 2:
                send(new, 1, string("seat0"))
    elif kind == "wl_compositor" and opcode == 0:
        objects[word(0)] = "wl_surface"
    elif kind == "wl_compositor" and opcode == 1:
        objects[word(0)] = "wl_region"
    elif kind == "wl_shm" and opcode == 0:
        pools[word(0)] = incoming_fds.pop(0)
        objects[word(0)] = "wl_shm_pool"
    elif kind == "wl_shm_pool" and opcode == 0:
        new, offset, width, height, stride, _ = struct.unpack_from("<IiiiiI", body)
        objects[new] = "wl_buffer"
        buffers[new] = (pools[object_id], offset, width, height, stride)
    elif kind == "wl_buffer" and opcode == 0:
        buffers.pop(object_id, None)
    elif kind == "wl_subcompositor" and opcode == 1:
        new, surface = word(0), word(4)
        objects[new] = "wl_subsurface"
        subsurfaces.append([new, surface, 0, 0])
        log(f"subsurface {new} created")
    elif kind == "wl_subsurface":
        entry = next((entry for entry in subsurfaces if entry[0] == object_id), None)
        if opcode == 0 and entry is not None:
            subsurfaces.remove(entry)
            contents.pop(entry[1], None)
            log(f"subsurface {object_id} destroyed")
            if window is not None and window[1] in contents:
                frame["pixels"] = compose()
                frame["hash"] = hashlib.md5(frame["pixels"]).digest()
        elif opcode == 1 and entry is not None:
            x, y = struct.unpack_from("<ii", body)
            del entry[4:]
            entry.append((x, y))
    elif kind == "wl_surface":
        if opcode == 9:
            x, y, w, h = struct.unpack_from("<iiii", body)
            old = damage.get(object_id)
            if old:
                x0, y0 = min(x, old[0]), min(y, old[1])
                x1, y1 = max(x + w, old[0] + old[2]), max(y + h, old[1] + old[3])
                x, y, w, h = x0, y0, x1 - x0, y1 - y0
            damage[object_id] = [x, y, w, h]
        elif opcode == 0:
            contents.pop(object_id, None)
        elif opcode == 1:
            surfaces[object_id] = word(0)
        elif opcode == 3:
            callback = word(0)
            send(callback, 0, struct.pack("<I", ms()))
            send(1, 1, struct.pack("<I", callback))
        elif opcode == 6:
            commit(object_id)
    elif kind == "xdg_wm_base":
        if opcode == 2:
            xdg = word(0)
            objects[xdg] = "xdg_surface"
            xdg_surfaces[xdg] = word(4)
        elif opcode == 3:
            log("pong", word(0))
    elif kind == "xdg_surface" and opcode == 1:
        toplevel = word(0)
        objects[toplevel] = "xdg_toplevel"
        toplevels[toplevel] = object_id
        window = (toplevel, xdg_surfaces[object_id])
    elif kind == "xdg_toplevel" and opcode == 0:
        log("toplevel destroyed")
    elif kind == "wl_seat":
        new = word(0) if opcode < 3 else None
        if opcode == 0:
            objects[new] = "wl_pointer"
            pointers.append(new)
        elif opcode == 1:
            objects[new] = "wl_keyboard"
            keyboards.append(new)
            memfd = os.memfd_create("keymap")
            os.write(memfd, KEYMAP)
            send(new, 0, struct.pack("<II", 1, len(KEYMAP)), fds=[memfd])
            os.close(memfd)
            if seat_version >= 4:
                send(new, 5, struct.pack("<ii", 0, 0))
        elif opcode == 2:
            objects[new] = "wl_touch"
            touches.append(new)


while True:
    readable, _, _ = select.select([connection, fifo], [], [])
    if fifo in readable:
        commands += os.read(fifo, 4096)
        while b"\n" in commands:
            line, commands = commands.split(b"\n", 1)
            if line.strip():
                try:
                    run(line.decode().strip())
                except OSError:
                    sys.exit(0)
    if connection not in readable:
        continue
    try:
        chunk, ancillary, _, _ = connection.recvmsg(65536, socket.CMSG_LEN(16 * 4))
    except OSError:
        break
    if not chunk:
        log("client gone")
        break
    for level, kind, payload in ancillary:
        if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
            incoming_fds += [struct.unpack_from("<i", payload, i)[0] for i in range(0, len(payload) - len(payload) % 4, 4)]
    data += chunk
    while len(data) >= 8:
        object_id, header = struct.unpack_from("<II", data)
        size = header >> 16
        if size < 8 or len(data) < size:
            break
        try:
            handle(object_id, header & 0xFFFF, data[8:size])
        except OSError:
            sys.exit(0)
        data = data[size:]
