"""生成 assets/app.ico —— 程序自己的图标。

为什么要自己画而不是拿一张图去转：托盘里那个电量环是**代码画出来的**
（src/main.rs 的 ring_icon），图标必须和它长得一样 —— 用户在资源管理器里
看到的环，和在托盘里看到的环，得是同一个东西。改那个环的几何参数时，
这里的常量也要跟着改。

只依赖标准库：PNG 用 zlib 压缩原始扫描线，ICO 只是几个头 + 拼起来的 PNG。
Vista 起 ICO 允许内嵌 PNG（比 BMP 小得多，还能带 alpha），Win10/11 没问题。

用法：python tools/make_icon.py
"""

import math
import os
import struct
import zlib

# 与托盘图标一致的几何：32px 上外半径 15.2、内半径 10.8，即直径的 0.475 / 0.3375
R_OUT = 0.475
R_IN = 0.330  # 比托盘略粗一点点：小尺寸下细环会糊成一团

# 与 src/main.rs 的 color_of() 一致
FILL = (39, 174, 96)  # 正常档的绿
# 底环用中性灰：资源管理器里白底、深色底都有人用，偏哪边都不合适
DIM = (128, 128, 128)

PERCENT = 78  # 留个缺口，一眼看得出是「环形进度」而不是一个圆圈

# 填充起点：0 = 12 点方向，90 = 3 点方向（顺时针为正）。
# 取 90 是为了让那 22% 的缺口落在**右上**：填充从 3 点起顺时针绕一圈，
# 缺口就停在 12 点到 3 点之间。
# 这里刻意与托盘图标不同 —— 托盘的环从 12 点起填充，缺口随电量在末段游走，
# 那是「进度」该有的样子；而 exe 图标是固定不动的标识，缺口放正上方偏左会显得
# 像没画完，放右上更稳。别把两边改回一致。
START_DEG = 90.0

SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
SS = 4  # 每个输出像素横竖各采 4 个样点做抗锯齿


def chunk(typ: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + typ
        + data
        + struct.pack(">I", zlib.crc32(typ + data) & 0xFFFFFFFF)
    )


def png(width: int, height: int, rows: list) -> bytes:
    raw = b"".join(b"\x00" + bytes(r) for r in rows)
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)  # 8 位 RGBA
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def draw(size: int) -> bytes:
    c = (size - 1) / 2.0
    r_out = R_OUT * size
    r_in = R_IN * size
    filled = PERCENT / 100.0 * 360.0

    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            cov_fill = cov_dim = 0
            for sy in range(SS):
                for sx in range(SS):
                    # 采样点取子像素中心，避免整点正好压在边界上
                    px = x + (sx + 0.5) / SS
                    py = y + (sy + 0.5) / SS
                    dx, dy = px - c, py - c
                    d = math.hypot(dx, dy)
                    if d < r_in or d > r_out:
                        continue
                    # 以 12 点方向为 0°，顺时针为正（与托盘同一套约定）。
                    # atan2 的 dy 是**向下为正**（图像坐标），所以 12 点方向是 -90°，
                    # 换成「从 12 点起顺时针」要减 90 —— 加 90 会画成逆时针
                    ang = math.degrees(math.atan2(dy, dx)) - 90.0
                    # 再减掉起点，把「从 START_DEG 起」换算成「从 0 起」，
                    # 取模保证跨过 360 的那段（尾段绕回起点前）也判成已填充
                    rel = (ang - START_DEG) % 360.0
                    if rel <= filled:
                        cov_fill += 1
                    else:
                        cov_dim += 1
            total = cov_fill + cov_dim
            if total == 0:
                row += b"\x00\x00\x00\x00"
                continue
            a = 255.0 * total / (SS * SS)
            mix = lambda i: (
                FILL[i] * cov_fill + DIM[i] * cov_dim
            ) / total
            row += bytes([round(mix(0)), round(mix(1)), round(mix(2)), round(a)])
        rows.append(row)
    return png(size, size, rows)


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    out = os.path.join(here, os.pardir, "assets", "app.ico")
    os.makedirs(os.path.dirname(out), exist_ok=True)

    blobs = [(s, draw(s)) for s in SIZES]

    # ICONDIR(6) + 每个尺寸一条 ICONDIRENTRY(16)，之后是各图数据
    offset = 6 + 16 * len(blobs)
    head = struct.pack("<HHH", 0, 1, len(blobs))
    entries = b""
    for s, data in blobs:
        entries += struct.pack(
            "<BBBBHHII",
            s % 256,  # 256 在这里写 0
            s % 256,
            0,  # 调色板色数：真彩色为 0
            0,
            1,  # planes
            32,  # bit count
            len(data),
            offset,
        )
        offset += len(data)

    with open(out, "wb") as f:
        f.write(head + entries + b"".join(d for _, d in blobs))
    print("写出 %s（%d 个尺寸，%.1f KB）" % (os.path.normpath(out), len(blobs), os.path.getsize(out) / 1024))


main()
