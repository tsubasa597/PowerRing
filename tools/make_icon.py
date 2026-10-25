"""生成 assets/app.ico —— 程序自己的图标。

为什么要自己画而不是拿一张图去转：托盘里那个电量环是**代码画出来的**
（src/main.rs 的 ring_icon），图标得和它同源 —— 同样的半径、同样的档位色、
同样的「12 点起顺时针」方向。改那个环的几何或配色时，这里的常量也要跟着改。

一个刻意的差别：托盘画的是**某一刻的电量**（填到 78% 就停），图标画的是
**整圈三段档位色**。exe 图标是固定标识，不该显示 78% 这种具体读数。

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

# 三段档位色，与 src/main.rs 的 color_of() 逐字对应。
#
# 整圈都填满，不画「未填充的灰底」、也不留缺口 —— 缺口的作用是让人看出这是个
# 「环形进度条」，而把三段档位色沿环铺开已经说明了同一件事，还顺便说明了
# 「它的颜色会随电量变」。留缺口反而让人以为环没画完。
#
# 从 12 点起顺时针 = 电量从 0% 涨到 100%，依次红 → 橙 → 绿。
# 边界用设置窗口的**默认**阈值（10 / 20）：图标是编译进 exe 的固定资源，
# 不可能跟着用户改过的阈值变，取默认值才算得上「和程序一致」。
#
# 充电的蓝**不放进来**：那是状态，不是一段电量区间。硬塞一段蓝会毁掉
# 「环 = 电量」这个读法 —— 蓝该摆在哪一段？没有答案。
CRITICAL_PCT = 10
WARN_PCT = 20
COLORS = [
    (192, 57, 43),  # 严重：红
    (230, 126, 34),  # 警告：橙
    (39, 174, 96),  # 正常：绿
]

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


def segment(ang: float) -> int:
    """钟面角（0 = 12 点，顺时针）落在哪一档。"""
    pct = ang / 360.0 * 100.0
    if pct < CRITICAL_PCT:
        return 0
    if pct < WARN_PCT:
        return 1
    return 2


def draw(size: int) -> bytes:
    c = (size - 1) / 2.0
    r_out = R_OUT * size
    r_in = R_IN * size

    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            hits = [0, 0, 0]  # 每个采样点落在哪一档
            for sy in range(SS):
                for sx in range(SS):
                    # 采样点取子像素中心，避免整点正好压在边界上
                    px = x + (sx + 0.5) / SS
                    py = y + (sy + 0.5) / SS
                    dx, dy = px - c, py - c
                    d = math.hypot(dx, dy)
                    if d < r_in or d > r_out:
                        continue
                    # 换算成钟面角：0 = 12 点，顺时针为正（和托盘 ring_icon 同一套）。
                    # atan2 的 dy **向下为正**（图像坐标），12 点方向算出来是 -90°，
                    # 要挪到 0 得**加** 90 —— 写成减 90 会把整个环转 180°，
                    # 即「红段在正上方」变成「红段在正下方」，看不出错但方向全反
                    ang = math.degrees(math.atan2(dy, dx)) + 90.0
                    if ang < 0.0:
                        ang += 360.0
                    hits[segment(ang)] += 1
            total = hits[0] + hits[1] + hits[2]
            if total == 0:
                row += b"\x00\x00\x00\x00"
                continue
            a = 255.0 * total / (SS * SS)
            mix = lambda i: sum(COLORS[k][i] * hits[k] for k in range(3)) / total
            row += bytes([round(mix(0)), round(mix(1)), round(mix(2)), round(a)])
        rows.append(row)
    return png(size, size, rows)


def check(size: int, blob: bytes) -> None:
    """把生成结果**读回来**验一遍，确认三段的位置没跑偏。

    为什么要读回来验：角度换算的符号写错（+90 写成 -90）不会报错，
    只会让整个环转 180° —— 肉眼看是个「环」，位置却全反了。
    采样公式这里刻意绕开 draw() 里那套，免得两边一起错还互相验证通过。
    """
    pos = 8
    idat = b""
    while pos < len(blob):
        ln = struct.unpack_from(">I", blob, pos)[0]
        typ = blob[pos + 4 : pos + 8]
        if typ == b"IDAT":
            idat += blob[pos + 8 : pos + 8 + ln]
        pos += 12 + ln
    raw = zlib.decompress(idat)
    stride = 1 + size * 4
    c = (size - 1) / 2.0
    r = (R_OUT + R_IN) / 2 * size

    def at(deg: float) -> tuple:
        # deg 是「12 点起顺时针」，这里换成图像坐标（y 向下为正）
        a = math.radians(deg)
        x = int(round(c + r * math.sin(a)))
        y = int(round(c - r * math.cos(a)))
        row = raw[y * stride + 1 : (y + 1) * stride]
        return tuple(row[x * 4 : x * 4 + 4])

    def nearest(px: tuple) -> int:
        return min(range(3), key=lambda k: sum((px[i] - COLORS[k][i]) ** 2 for i in range(3)))

    # 取每段的**中心**采样：段边界上有抗锯齿混色，取中心才判得准
    cases = [
        (CRITICAL_PCT / 2 / 100 * 360, 0, "正上方那一段应该是红"),
        ((CRITICAL_PCT + WARN_PCT) / 2 / 100 * 360, 1, "接着那一段应该是橙"),
        ((WARN_PCT + 100) / 2 / 100 * 360, 2, "剩下大半圈应该是绿"),
    ]
    for deg, want, why in cases:
        got = nearest(at(deg))
        assert got == want, "%s：算出来是第 %d 档（%s）" % (why, got, COLORS[got])

    # 中间必须是镂空的：ring 写成实心圆的话，一眼看去就不再是「环」
    cx = cy = int(round(c))
    center = raw[cy * stride + 1 : (cy + 1) * stride][cx * 4 + 3]
    assert center == 0, "圆心应该是透明的，实际 alpha=%d" % center
    print("自检通过：红在上、橙接后、绿占大半圈，圆心镂空")


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    out = os.path.join(here, os.pardir, "assets", "app.ico")
    os.makedirs(os.path.dirname(out), exist_ok=True)

    blobs = [(s, draw(s)) for s in SIZES]
    check(SIZES[-1], blobs[-1][1])

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
