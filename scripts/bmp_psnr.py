#!/usr/bin/env python3
"""比较两张 BMP（peerdesk 写出的 24 位无压缩格式）的亮度 PSNR，打印 dB。

用法：bmp_psnr.py <a.bmp> <b.bmp> [最低 dB]
给了最低值时，低于它就以非零状态退出。尺寸不同算失败。只用标准库。
"""
import math
import struct
import sys


def load(path):
    with open(path, "rb") as f:
        data = f.read()
    if data[:2] != b"BM":
        sys.exit(f"{path}: 不是 BMP")
    offset, = struct.unpack_from("<I", data, 10)
    width, height = struct.unpack_from("<ii", data, 18)
    bpp, = struct.unpack_from("<H", data, 28)
    if bpp not in (24, 32):
        sys.exit(f"{path}: 只支持 24/32 位，实际 {bpp}")
    step = bpp // 8
    row = (width * step + 3) & ~3
    luma = []
    for y in range(abs(height)):
        base = offset + y * row
        for x in range(width):
            b, g, r = data[base + x * step: base + x * step + 3]
            luma.append(0.299 * r + 0.587 * g + 0.114 * b)
    return width, abs(height), luma


def main():
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    wa, ha, a = load(sys.argv[1])
    wb, hb, b = load(sys.argv[2])
    if (wa, ha) != (wb, hb):
        print(f"尺寸不同：{wa}x{ha} 和 {wb}x{hb}")
        sys.exit(1)
    mse = sum((x - y) ** 2 for x, y in zip(a, b)) / len(a)
    psnr = 99.0 if mse < 1e-9 else 10 * math.log10(255 * 255 / mse)
    print(f"{psnr:.1f}")
    if len(sys.argv) == 4 and psnr < float(sys.argv[3]):
        sys.exit(1)


main()
