#!/usr/bin/env python3
"""A scripted Wayland compositor that sends events at exact protocol moments, which Weston cannot do."""
# Minimal scripted Wayland compositor for protocol-level probes. usage: fakewl.py <socket> <scenario>
import os, select, socket, struct, sys, time
path, scenario = sys.argv[1], sys.argv[2]
try: os.unlink(path)
except OSError: pass
srv = socket.socket(socket.AF_UNIX); srv.bind(path); srv.listen(1)
conn, _ = srv.accept(); conn.setblocking(True)
buf = b""; fds = []
objs = {1: "wl_display"}; bufs = {}; pools = {}
serial = 100; commits = 0; attached = None; xsurf = top = wm = None; roundtrips_after_commit = 0
GLOBALS = [(1, "wl_compositor", 4), (2, "wl_shm", 1), (3, "xdg_wm_base", 1)]
if scenario == "no-wm": GLOBALS = GLOBALS[:2]
def s(t):
    b = t.encode() + b"\0"; return struct.pack("<I", len(b)) + b + b"\0" * (-len(b) % 4)
def send(oid, op, body=b""):
    conn.sendall(struct.pack("<II", oid, ((8 + len(body)) << 16) | op) + body)
def log(*a): print("[compositor]", *a, flush=True)
def configure(w, h):
    global serial
    serial += 1
    send(top, 0, struct.pack("<ii", w, h) + struct.pack("<I", 0))
    send(xsurf, 0, struct.pack("<I", serial))
    log(f"sent configure {w}x{h}")
def close():
    send(top, 1); log("sent close")
timers = []
def later(sec, fn): timers.append((time.time() + sec, fn))
def on_commit():
    global commits
    if attached is None: 
        if xsurf and top and commits == 0:
            w, h = (20000, 20000) if scenario == "initial-huge" else (0, 0)
            configure(w, h)
        return
    commits += 1
    fd, off, w, h, stride = bufs[attached]
    corner = os.pread(fd, 4, off + stride * (h - 1) + (w - 1) * 4)
    log(f"frame {commits}: buffer {w}x{h}, last pixel {corner.hex()}")
    if scenario == "resize":
        if commits == 1: later(0.3, lambda: configure(1000, 700))
        elif commits == 2: later(0.3, lambda: configure(320, 200))
        elif commits == 3: later(0.3, lambda: configure(1, 1))
        elif commits == 4: later(0.3, close)
    if scenario == "huge" and commits == 1: later(0.3, lambda: configure(20000, 20000))
    if scenario == "ping" and commits == 1:
        later(0.1, lambda: send(wm, 0, struct.pack("<I", 777))); later(0.5, close)
    if scenario == "close" and commits == 1: later(0.3, close)
    if scenario == "error" and commits == 1: later(0.3, lambda: send(1, 0, struct.pack("<II", top, 1) + s("deliberate")))
    if scenario == "race-resize" and commits == 2: later(2.0, close)
def on_sync_after_commit():
    # The client's present() does commit then sync; what arrives before `done` is consumed in its roundtrip.
    global raced
    if commits == 1 and not raced:
        raced = True
        if scenario == "race-close": close()
        if scenario == "race-resize":
            configure(1000, 700); later(3.0, lambda: (log("no redraw in 3 s, now closing"), close()))
raced = False
def handle(oid, op, body):
    global attached, xsurf, top, wm
    it = objs.get(oid, "?")
    u = lambda i: struct.unpack_from("<I", body, i)[0]
    if it == "wl_display":
        if op == 0:
            new = u(0); on_sync_after_commit(); send(new, 0, struct.pack("<I", 0)); send(1, 1, struct.pack("<I", new))
        elif op == 1:
            new = u(0); objs[new] = "wl_registry"
            for name, iface, ver in GLOBALS: send(new, 0, struct.pack("<I", name) + s(iface) + struct.pack("<I", ver))
    elif it == "wl_registry":
        name = u(0); ln = u(4); iface = body[8:8 + ln - 1].decode(); p = 8 + ln + (-ln % 4)
        ver, new = struct.unpack_from("<II", body, p); objs[new] = iface
        if iface == "wl_shm": send(new, 0, struct.pack("<I", 1)); send(new, 0, struct.pack("<I", 0))
        if iface == "xdg_wm_base": wm = new
    elif it == "wl_compositor" and op == 0: objs[u(0)] = "wl_surface"
    elif it == "wl_shm" and op == 0:
        pools[u(0)] = fds.pop(0); objs[u(0)] = "wl_shm_pool"
    elif it == "wl_shm_pool" and op == 0:
        new, off, w, h, stride, fmt = struct.unpack_from("<IiiiiI", body); objs[new] = "wl_buffer"; bufs[new] = (pools[oid], off, w, h, stride)
    elif it == "wl_buffer" and op == 0: log(f"buffer {oid} destroyed"); bufs.pop(oid, None); attached = None if attached == oid else attached
    elif it == "wl_surface":
        if op == 1:
            attached = u(0)
            if attached not in bufs: log("PROTOCOL BUG: attach of unknown buffer", attached)
        if op == 6: on_commit()
    elif it == "xdg_wm_base":
        if op == 2: xsurf = u(0); objs[xsurf] = "xdg_surface"
        if op == 3: log("pong", u(0))
    elif it == "xdg_surface":
        if op == 1: top = u(0); objs[top] = "xdg_toplevel"
        if op == 4: log("ack_configure", u(0))
while True:
    t = min((x[0] for x in timers), default=None)
    r, _, _ = select.select([conn], [], [], None if t is None else max(0, t - time.time()))
    now = time.time()
    for x in [x for x in timers if x[0] <= now]:
        timers.remove(x)
        try: x[1]()
        except OSError: sys.exit(0)
    if not r: continue
    try: data, anc, _, _ = conn.recvmsg(65536, socket.CMSG_LEN(64))
    except OSError: break
    if not data: log("client gone"); break
    for lvl, typ, c in anc:
        if lvl == socket.SOL_SOCKET and typ == socket.SCM_RIGHTS:
            fds += [struct.unpack_from("<i", c, i)[0] for i in range(0, len(c) - len(c) % 4, 4)]
    buf += data
    while len(buf) >= 8:
        oid, w = struct.unpack_from("<II", buf); size = w >> 16
        if len(buf) < size: break
        handle(oid, w & 0xffff, buf[8:size]); buf = buf[size:]
