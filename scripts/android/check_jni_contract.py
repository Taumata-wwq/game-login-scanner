#!/usr/bin/env python3
"""Kotlin <-> Rust JNI 契约静态检查（不需要 Android SDK / NDK / Gradle / 模拟器）。

背景：Kotlin 的 `external fun` 与 Rust 的 `#[no_mangle] pub extern "system" fn
Java_...` 只靠**符号名**绑定，编译期完全不校验。任何一侧改包名、类名或方法名，构建
照样成功，直到真机运行才炸 `UnsatisfiedLinkError`；R8 还会把没有 `@Keep` 的桥接类
整类删掉（同样只在运行期暴露）。这类事故本项目已经踩过两次（2026-10-17 通知桥、
2026-10-21 相机桥），故把下列不变量提前到 CI：

1. 每个 Kotlin `external fun` 都有同名 Rust JNI 符号，反之亦然；
2. 声明了 `external fun` 的文件必须带 `@Keep`（否则 R8 移除后在运行期报错）；
3. 该文件必须 `System.loadLibrary("game_login_scanner_lib")`（否则 nativeInit 找不到库）；
4. Kotlin 包名必须是 `com.taumata.scanner.plugins`（JNI 符号名硬编码了它；且包路径
   不能含下划线，否则 JNI 规范要求转义成 `_1`）。

用法：`python scripts/android/check_jni_contract.py`（任意工作目录均可）。
退出码 0 = 通过，1 = 有不一致。
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# CI（GitHub Actions 的 windows-latest）默认 stdout 编码是 cp1252，打印中文会在
# print() 里抛 UnicodeEncodeError——那会把「检查通过/失败」淹没成环境报错。此处先把
# 两个流固定成 UTF-8（不支持 reconfigure 的老环境退化为原行为）。
for _stream in (sys.stdout, sys.stderr):
    if hasattr(_stream, "reconfigure"):
        _stream.reconfigure(encoding="utf-8", errors="replace")

ROOT = Path(__file__).resolve().parents[2]
KT_DIRS = [
    ROOT / "src-tauri" / "android-plugin",
    ROOT / "src-tauri" / "android-plugin-huawei",
]
RUST_SRC = ROOT / "src-tauri" / "src" / "android" / "jni_bridge.rs"
PKG = "com.taumata.scanner.plugins"
LIB_NAME = "game_login_scanner_lib"

# 顶层 object/class 声明（Kotlin 里本项目的桥接类都写在行首，无缩进）。
RE_DECL = re.compile(r"^(?:internal\s+|public\s+)?(?:object|class)\s+(\w+)")
# external fun 声明（可带 private/internal）。
RE_EXT = re.compile(r"^\s*(?:private\s+|internal\s+)?external\s+fun\s+(\w+)\s*\(")
RE_PKG = re.compile(r"^\s*package\s+([\w.]+)", re.M)

problems: list[str] = []
kotlin_syms: dict[tuple[str, str], str] = {}


def scan_kotlin() -> None:
    for d in KT_DIRS:
        if not d.is_dir():
            problems.append(f"[!] 目录不存在：{d.relative_to(ROOT)}")
            continue
        for path in sorted(d.glob("*.kt")):
            rel = path.relative_to(ROOT).as_posix()
            text = path.read_text(encoding="utf-8")
            m = RE_PKG.search(text)
            pkg = m.group(1) if m else ""
            cur = None
            keep_pending = False
            cur_keep = False
            funs: list[str] = []
            for line in text.splitlines():
                if line.startswith("@Keep"):
                    keep_pending = True
                    continue
                decl = RE_DECL.match(line)
                if decl:
                    cur = decl.group(1)
                    cur_keep = keep_pending
                    keep_pending = False
                    continue
                ext = RE_EXT.match(line)
                if ext and cur:
                    funs.append(ext.group(1))
            if not funs:
                continue
            if pkg != PKG:
                problems.append(
                    f"[!] {rel}: 包名是 {pkg or '(缺失)'}，JNI 符号要求 {PKG}"
                )
            if not cur_keep:
                problems.append(
                    f"[!] {rel}: {cur} 声明了 external fun 却没有 @Keep（R8 会删掉它）"
                )
            if f'System.loadLibrary("{LIB_NAME}")' not in text:
                problems.append(
                    f"[!] {rel}: 缺少 System.loadLibrary(\"{LIB_NAME}\")"
                )
            for fn in funs:
                kotlin_syms[(cur or "?", fn)] = rel


def scan_rust() -> dict[tuple[str, str], str]:
    rel = RUST_SRC.relative_to(ROOT).as_posix()
    text = RUST_SRC.read_text(encoding="utf-8")
    prefix = "Java_" + PKG.replace(".", "_") + "_"
    out: dict[tuple[str, str], str] = {}
    for line in text.splitlines():
        if "fn " + prefix not in line:
            continue
        start = line.index("fn " + prefix) + len("fn " + prefix)
        rest = line[start:]
        # 截到第一个非标识符字符（'<' / '(' 等）
        mm = re.match(r"[A-Za-z0-9_]+", rest)
        if not mm:
            problems.append(f"[!] {rel}: 无法解析 JNI 符号：{line.strip()}")
            continue
        sym = mm.group(0)
        # 重载后缀 Java_..._method__II 形式（本项目未用，出现即提示人工确认）
        if "__" in sym:
            problems.append(f"[!] {rel}: 存在重载符号 {sym}，本脚本无法覆盖，请人工确认")
            sym = sym.split("__", 1)[0]
        cls, _, meth = sym.partition("_")
        out[(cls, meth)] = rel
    return out


def main() -> int:
    scan_kotlin()
    rust_syms = scan_rust()

    for key in sorted(set(kotlin_syms) - set(rust_syms)):
        problems.append(
            f"[!] Kotlin 声明了 {key[0]}.{key[1]}（{kotlin_syms[key]}），"
            f"但 Rust 侧没有符号 Java_{PKG.replace('.', '_')}_{key[0]}_{key[1]}"
        )
    for key in sorted(set(rust_syms) - set(kotlin_syms)):
        problems.append(
            f"[!] Rust 导出了 {key[0]}.{key[1]}（{rust_syms[key]}），但 Kotlin 侧没有对应的 external fun"
        )

    if problems:
        for p in problems:
            print(p)
        print(f"\n[FAIL] JNI 契约检查未通过（{len(problems)} 项）")
        return 1

    inits = sum(1 for (_c, m) in kotlin_syms if m == "nativeInit")
    print(
        f"[OK] JNI 契约一致：{len(kotlin_syms)} 个符号"
        f"（{inits} 个 nativeInit + {len(kotlin_syms) - inits} 个回调），"
        f"@Keep / loadLibrary / 包名 均符合约定"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
