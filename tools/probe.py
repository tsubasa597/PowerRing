#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""PowerRing 设备探测助手

不知道自己的设备该怎么写那份 YAML？这个脚本帮你把要先弄清楚的事实挖出来：

  list   列出本机所有 HID 接口，拿到 vendor_id / product_id / usage_page / usage
  scan   对指定设备的每个接口逐个试请求报文，找出「哪个接口会回应、回的是什么」

依赖： pip install hidapi

用法示例：
  python tools/probe.py list
  python tools/probe.py list --vid 0x0951
  python tools/probe.py scan --vid 0x0951 --pid 0x1723
  python tools/probe.py scan --vid 0x0951 --pid 0x1723 --cmds "21 FF 05,06 FF 05"

拿到结果后照着 devices/ 里最接近的声明改，详见 docs/adding-a-device.md。
"""
from __future__ import annotations

import argparse
import sys
import time

try:
    import hid
except ImportError:
    sys.exit("缺少依赖，先装一下：  pip install hidapi")

# Windows 控制台默认 GBK，直接 print 中文会炸
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")


def hexint(v) -> str:
    return "0x%04X" % (v or 0)


def hexrow(row) -> str:
    return " ".join("%02X" % b for b in row)


def open_path(path):
    """以非阻塞方式打开一个 HID 接口。系统占用了的接口会抛异常，跳过即可。"""
    d = hid.device()
    d.open_path(path)
    d.set_nonblocking(1)
    return d


def ask(d, payload: bytes, timeout: float = 0.15):
    """发一个报文并在超时窗口内等响应。返回 list[int] 或 None。"""
    for _ in range(64):          # 先把旧数据排空，否则读到的是上一轮的响应
        if not d.read(64, 5):
            break
    try:
        d.write(payload)
    except OSError:
        return None
    end = time.time() + timeout
    while time.time() < end:
        r = d.read(64, 20)
        if r:
            return list(r)
    return None


def cmd_list(args):
    devs = hid.enumerate()
    if args.vid:
        devs = [d for d in devs if d.get("vendor_id") == args.vid]
    if args.pid:
        devs = [d for d in devs if d.get("product_id") == args.pid]
    if not devs:
        print("没有匹配到的 HID 接口。不填 --vid/--pid 可以看全部。")
        return 1

    cur = None
    for d in devs:
        key = (d.get("vendor_id"), d.get("product_id"))
        if key != cur:
            cur = key
            print("\n== %s:%s  %s" % (
                hexint(d.get("vendor_id")), hexint(d.get("product_id")),
                d.get("product_string") or ""))
        # 同一个 USB 设备往往暴露好几个接口，电池请求通常只在其中一个上有回应，
        # 所以每个都要单独试，这也是为什么每份声明都要写 usage_page / usage
        print("   usage_page=%s usage=%s  %s" % (
            hexint(d.get("usage_page")), hexint(d.get("usage")),
            (d.get("interface_number") is not None
             and "iface=%d" % d["interface_number"] or "")))
    print("\n共 %d 个接口。" % len(devs))
    return 0


def cmd_scan(args):
    devs = [d for d in hid.enumerate()
            if d.get("vendor_id") == args.vid
            and (args.pid is None or d.get("product_id") == args.pid)]
    if not devs:
        print("没找到 %s 的接口。先用 list 看一眼它在不在。" % hexint(args.vid))
        return 1

    cmds: list[bytes] = []
    for c in args.cmds.split(","):
        c = c.strip()
        if c:
            cmds.append(bytes(int(x, 16) for x in c.split()))
    if not cmds:
        # 没指定就扫 report id：每个候选 id 配两个常见问询字节序
        for rid in range(0x00, 0x31):
            cmds.append(bytes([rid, 0xFF, 0x05]))
            cmds.append(bytes([rid]))
    print("每个接口要试 %d 条请求，接口共 %d 个，耐心等一会儿…\n" % (len(cmds), len(devs)))

    found = 0
    for d in devs:
        up = hexint(d.get("usage_page"))
        us = hexint(d.get("usage"))
        try:
            h = open_path(d["path"])
        except OSError as e:
            print("-- usage_page=%s usage=%s  打不开（可能被系统独占）：%s" % (up, us, e))
            continue

        hits = []
        for c in cmds:
            buf = c + bytes(args.len - len(c)) if len(c) < args.len else c
            r = ask(h, buf, args.timeout)
            if r:
                hits.append((hexrow(c), hexrow(r)))
        try:
            h.close()
        except OSError:
            pass

        if hits:
            found += len(hits)
            print("## usage_page=%s usage=%s  有 %d 条请求得到响应" % (up, us, len(hits)))
            for sent, got in hits:
                print("   发 %-24s -> 回 %s" % (sent, got))
        else:
            print("-- usage_page=%s usage=%s  全部无响应" % (up, us))

    print("\n有响应的组合共 %d 条。" % found)
    if not found:
        print("换一组报文字节再试：很多设备的电量请求是自己定义的命令字，"
              "先在网上搜一下设备名 + hid / battery report。")
    return 0


def main():
    p = argparse.ArgumentParser(description="PowerRing 设备探测助手")
    p.add_argument("--timeout", type=float, default=0.15, help="单次请求等待响应的秒数")
    p.add_argument("--len", type=int, default=20, help="请求报文补齐到多少字节（很多设备按长度过滤）")
    sub = p.add_subparsers(dest="cmd", required=True)

    l = sub.add_parser("list", help="列出 HID 接口")
    l.add_argument("--vid", type=lambda x: int(x, 16))
    l.add_argument("--pid", type=lambda x: int(x, 16))
    l.set_defaults(fn=cmd_list)

    s = sub.add_parser("scan", help="对指定设备的每个接口试请求")
    s.add_argument("--vid", type=lambda x: int(x, 16), required=True)
    s.add_argument("--pid", type=lambda x: int(x, 16))
    s.add_argument("--cmds", default="", help='逗号分隔的请求字节串，如 "21 FF 05,06 FF 05"')
    s.set_defaults(fn=cmd_scan)

    args = p.parse_args()
    return args.fn(args)


if __name__ == "__main__":
    raise SystemExit(main())
