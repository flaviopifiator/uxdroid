"""Checks a screenshot for the test APK's color and text. Uses only the standard library."""
import struct
import sys
import zlib

TEAL = (0, 128, 128)
WHITE = (255, 255, 255)
MIN_TEAL_PIXELS = 300_000
MIN_TEXT_PIXELS = 500
TOLERANCE = 3


def read_png(path):
    data = open(path, "rb").read()
    position = 8
    idat = b""
    while position < len(data):
        length, kind = struct.unpack(">I4s", data[position : position + 8])
        body = data[position + 8 : position + 8 + length]
        if kind == b"IHDR":
            width, height, depth, color = struct.unpack(">IIBB", body[:10])
        elif kind == b"IDAT":
            idat += body
        position += 12 + length
    if depth != 8 or color not in (2, 6):
        raise SystemExit(f"unexpected PNG format: depth {depth}, color {color}")
    channels = 3 if color == 2 else 4
    raw = zlib.decompress(idat)
    stride = width * channels
    rows = []
    previous = bytearray(stride)
    offset = 0
    for _ in range(height):
        kind = raw[offset]
        row = bytearray(raw[offset + 1 : offset + 1 + stride])
        offset += stride + 1
        for i in range(stride):
            left = row[i - channels] if i >= channels else 0
            up = previous[i]
            corner = previous[i - channels] if i >= channels else 0
            if kind == 1:
                row[i] = (row[i] + left) & 255
            elif kind == 2:
                row[i] = (row[i] + up) & 255
            elif kind == 3:
                row[i] = (row[i] + (left + up) // 2) & 255
            elif kind == 4:
                p = left + up - corner
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - corner)
                predictor = left if pa <= pb and pa <= pc else up if pb <= pc else corner
                row[i] = (row[i] + predictor) & 255
        rows.append(row)
        previous = row
    return width, height, channels, rows


width, height, channels, rows = read_png(sys.argv[1])
counts = {}
for row in rows:
    for x in range(0, width * channels, channels):
        pixel = (row[x], row[x + 1], row[x + 2])
        counts[pixel] = counts.get(pixel, 0) + 1


def near(pixel, target):
    # Weston's screenshot path shifts channels slightly: 128 arrives as 126.
    return all(abs(a - b) <= TOLERANCE for a, b in zip(pixel, target))


teal = sum(count for pixel, count in counts.items() if near(pixel, TEAL))
white = sum(count for pixel, count in counts.items() if near(pixel, WHITE))
top = sorted(counts.items(), key=lambda item: -item[1])[:4]
print(f"screenshot {width}x{height}: {teal} teal pixels, {white} white pixels, most common {top}")
if teal < MIN_TEAL_PIXELS:
    raise SystemExit("the window's background color is missing")
if white < MIN_TEXT_PIXELS:
    raise SystemExit("the text is missing")
