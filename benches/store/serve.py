# Serves the files of a directory over HTTP/1.1 on 127.0.0.1, one request per connection.
import os, socket, sys
root, port = sys.argv[1], int(sys.argv[2])
server = socket.socket(); server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", port)); server.listen(16)
while True:
    conn, _ = server.accept()
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(4096)
        if not chunk: break
        data += chunk
    path = data.split(b" ")[1].decode() if data.count(b" ") >= 2 else "/"
    name = os.path.normpath(path.lstrip("/"))
    file = os.path.join(root, name)
    if not name.startswith("..") and os.path.isfile(file):
        size = os.path.getsize(file)
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % size)
        with open(file, "rb") as f:
            while True:
                block = f.read(1 << 20)
                if not block: break
                conn.sendall(block)
    else:
        conn.sendall(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    conn.close()
