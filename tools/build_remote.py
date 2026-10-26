#!/usr/bin/env python3
"""把 devices/*.yaml 打成一份远程清单 remote/devices.yaml。

程序里的「检查设备声明更新…」只拉这一个文件 —— 一次请求拿全，
而不是一台设备一个请求。后者对连得上 GitHub 的人只是慢一点，
对连不上的人意味着要白等好几轮超时才肯罢休。

用法：
    python tools/build_remote.py          重新生成
    python tools/build_remote.py --check  只检查是不是最新的（CI 用，不一致就退出码 1）

别手改生成出来的文件：改了也会被下次生成覆盖。要加设备就改 devices/ 下的
声明文件，重新跑一次这个脚本，把生成结果一起提交。

为什么用脚本生成而不是手写：手写意味着每加一台设备要改两处，
忘一处的结果是「仓库里有这份声明，但别人同步不到」——
这种错误不会报错，只会安静地少一台设备。CI 里跑 --check 就是为了卡住它。
"""

import os
import sys

# CI 上跑这个脚本时 stdout 可能是 cp1252（GitHub 的 pwsh 默认代码页），
# 打印中文会直接抛 UnicodeEncodeError —— 「想说一句话」不该变成构建失败。
# 强制 UTF-8；万一还有打不出的字符，换成问号而不是让整个步骤红掉。
# （实测踩过：--check 其实已经通过了，崩在打印「是最新的」那一句上）
for _stream in (sys.stdout, sys.stderr):
    if hasattr(_stream, "reconfigure"):
        _stream.reconfigure(encoding="utf-8", errors="replace")

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEVICES = os.path.join(ROOT, "devices")
OUT = os.path.join(ROOT, "remote", "devices.yaml")

HEADER = """\
# 由 tools/build_remote.py 自动生成 —— 别手改，改了也会被下次生成覆盖。
# 要加设备：改 devices/ 下的声明文件，重新跑一次这个脚本，把结果一起提交。
#
# 这是托盘菜单里「检查设备声明更新…」要拉的那份清单：把 devices/ 里所有声明
# 打成一个文件，程序一次请求就能拿全。
# 注释在打包时被丢掉了：它们在仓库的 devices/*.yaml 里照样能读到，
# 而这份清单是给机器读的，多带几 KB 注释只是让同步白白变慢。
devices:
"""

# 每份声明至少要有这几样，少一样程序在编译声明时就会跳过它。
# 生成完自检一遍，免得把一个空壳或者写了一半的声明发出去
REQUIRED = ("name:", "match:", "vendor_id:", "product_id:", "protocol_source:")


def body_of(path):
    """取一份声明的正文：丢掉注释行与空行，整体缩进 4 空格。

    丢注释是为了让清单小一点，也为了避免「列表项第一行是注释」这种
    YAML 写法陷阱（`  - # xxx` 后面没有内容是非法的）。
    """
    out = []
    for line in open(path, encoding="utf-8").read().splitlines():
        s = line.strip()
        if not s or s.startswith("#"):
            continue
        out.append("    " + line.rstrip())
    return out


def build():
    """返回 (清单文本, 收进去的文件名列表)"""
    names = sorted(
        f for f in os.listdir(DEVICES) if f.endswith((".yaml", ".yml"))
    )
    items = []
    used = []
    for name in names:
        body = body_of(os.path.join(DEVICES, name))
        if not body:
            print(f"跳过 {name}：里面没有内容", file=sys.stderr)
            continue
        # 列表项：第一行换成 "  - " 打头，其余保持 4 空格缩进
        items.append("\n".join(["  - " + body[0].strip()] + body[1:]))
        used.append(name)
    # 排序决定顺序，不排序的话每次生成顺序都可能变，git diff 会平白多出一堆噪音
    return HEADER + "\n\n".join(items) + "\n", used


def check(text):
    """生成完自检：每一份都得有那几个关键字段"""
    bad = []
    for i, block in enumerate(text.split("\n  - ")[1:]):
        missing = [k for k in REQUIRED if k not in block]
        if missing:
            head = block.strip().splitlines()[0] if block.strip() else "(空)"
            bad.append(f"第 {i + 1} 份（{head}）缺 {', '.join(missing)}")
    return bad


def main():
    text, used = build()
    bad = check(text)
    if bad:
        print("生成的清单有问题，没有写出去：", file=sys.stderr)
        for b in bad:
            print("  - " + b, file=sys.stderr)
        return 1

    if "--check" in sys.argv:
        old = ""
        if os.path.exists(OUT):
            old = open(OUT, encoding="utf-8").read()
        if old != text:
            print(
                "remote/devices.yaml 不是最新的。加过或改过 devices/ 之后要跑："
                "python tools/build_remote.py",
                file=sys.stderr,
            )
            return 1
        print(f"remote/devices.yaml 是最新的（{len(used)} 份声明）")
        return 0

    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    # newline 必须写死 \n：Windows 下默认会写成 \r\n，
    # 拉下来比对的字节数、以及 git 的行尾都会跟着乱
    with open(OUT, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    print(
        f"已生成 remote/devices.yaml：{len(used)} 份声明，"
        f"{len(text.encode('utf-8'))} 字节"
    )
    for n in used:
        print(f"  - {n}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
