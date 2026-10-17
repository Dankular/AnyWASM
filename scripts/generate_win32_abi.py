#!/usr/bin/env python3
"""Generate verified x86 import cleanup metadata from PE imports and windows-sys prototypes.

This tool generates ABI metadata only. It never supplies HLE behavior. Imports without a
unambiguous prototype remain in the JSON report and continue to fail closed at runtime.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from collections import defaultdict


KNOWN_GROUPS = {
    "kernel32": "kernel32",
    "kernelbase": "kernel32",
    "user32": "user32",
    "gdi32": "gdi32",
    "advapi32": "advapi32",
    "shell32": "shell32",
    "shlwapi": "shlwapi",
    "ole32": "ole32",
    "winmm": "winmm",
    "dsound": "dsound",
    "d3d9": "d3d9",
    "dinput8": "dinput8",
    "ddraw": "ddraw",
    "dxgi": "dxgi",
    "d3d11": "d3d11",
    "ntdll": "ntdll",
}

PRIMITIVE_BYTES = {
    "i8": 1,
    "u8": 1,
    "bool": 1,
    "i16": 2,
    "u16": 2,
    "i32": 4,
    "u32": 4,
    "f32": 4,
    "isize": 4,
    "usize": 4,
    "i64": 8,
    "u64": 8,
    "f64": 8,
}


def find_windows_sys(explicit: str | None) -> Path:
    if explicit:
        root = Path(explicit).resolve()
        if (root / "Windows" / "Win32").is_dir():
            return root
        if (root / "src" / "Windows" / "Win32").is_dir():
            return root / "src"
        raise SystemExit(f"windows-sys source tree not found under {root}")

    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    candidates = sorted((cargo_home / "registry" / "src").glob("*"))
    for registry in candidates:
        versions = sorted(registry.glob("windows-sys-*/src/Windows/Win32"), reverse=True)
        if versions:
            return versions[0].parents[2]
    raise SystemExit("windows-sys source not found; pass --windows-sys PATH")


def split_top_level(text: str) -> list[str]:
    result: list[str] = []
    start = 0
    paren = angle = bracket = brace = 0
    quoted = escaped = False
    for i, ch in enumerate(text):
        if quoted:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                quoted = False
            continue
        if ch == '"':
            quoted = True
        elif ch == "(":
            paren += 1
        elif ch == ")":
            paren -= 1
        elif ch == "<":
            angle += 1
        elif ch == ">":
            angle = max(0, angle - 1)
        elif ch == "[":
            bracket += 1
        elif ch == "]":
            bracket -= 1
        elif ch == "{":
            brace += 1
        elif ch == "}":
            brace -= 1
        elif ch == "," and not (paren or angle or bracket or brace):
            result.append(text[start:i].strip())
            start = i + 1
    tail = text[start:].strip()
    if tail:
        result.append(tail)
    return result


def macro_records(src: Path):
    marker = "windows_link::link!("
    for path in src.rglob("*.rs"):
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError):
            continue
        pos = 0
        while True:
            begin = text.find(marker, pos)
            if begin < 0:
                break
            body_start = begin + len(marker)
            depth = 1
            quoted = escaped = False
            end = body_start
            while end < len(text) and depth:
                ch = text[end]
                if quoted:
                    if escaped:
                        escaped = False
                    elif ch == "\\":
                        escaped = True
                    elif ch == '"':
                        quoted = False
                elif ch == '"':
                    quoted = True
                elif ch == "(":
                    depth += 1
                elif ch == ")":
                    depth -= 1
                end += 1
            if depth:
                pos = body_start
                continue
            body = text[body_start : end - 1]
            head = re.match(
                r'\s*"(?P<dll>[^"]+)"\s+"(?P<abi>[^"]+)"\s+fn\s+'
                r'(?P<name>[A-Za-z_][A-Za-z0-9_@$]*)\s*\(',
                body,
            )
            if not head:
                pos = end
                continue
            args_start = head.end()
            depth = 1
            quoted = escaped = False
            cursor = args_start
            while cursor < len(body) and depth:
                ch = body[cursor]
                if quoted:
                    if escaped:
                        escaped = False
                    elif ch == "\\":
                        escaped = True
                    elif ch == '"':
                        quoted = False
                elif ch == '"':
                    quoted = True
                elif ch == "(":
                    depth += 1
                elif ch == ")":
                    depth -= 1
                cursor += 1
            if depth:
                pos = end
                continue
            args = body[args_start : cursor - 1]
            line = text.count("\n", 0, begin) + 1
            yield {
                "dll": Path(head.group("dll")).name.lower().removesuffix(".dll"),
                "abi": head.group("abi"),
                "name": head.group("name"),
                "args": split_top_level(args),
                "source": f"{path}:{line}",
            }
            pos = end


def aliases_in(src: Path) -> dict[str, set[str]]:
    aliases: dict[str, set[str]] = defaultdict(set)
    pattern = re.compile(r"\bpub\s+type\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([^;]+);")
    for path in src.rglob("*.rs"):
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError):
            continue
        for match in pattern.finditer(text):
            aliases[match.group(1)].add(match.group(2).strip())
    return aliases


def structs_in(src: Path) -> dict[str, list[str]]:
    structs: dict[str, list[str]] = {}
    marker = re.compile(r"\bpub\s+struct\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{")
    for path in src.rglob("*.rs"):
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError):
            continue
        for match in marker.finditer(text):
            # Only infer layout for explicitly C-compatible or transparent wrappers.
            if not re.search(r"#\[repr\((?:C|transparent)\)\]", text[max(0, match.start() - 256) : match.start()]):
                continue
            start = match.end()
            depth = 1
            end = start
            while end < len(text) and depth:
                if text[end] == "{":
                    depth += 1
                elif text[end] == "}":
                    depth -= 1
                end += 1
            if depth:
                continue
            fields = []
            for field in split_top_level(text[start : end - 1]):
                if ":" not in field:
                    continue
                _, ty = field.split(":", 1)
                fields.append(ty.strip())
            structs.setdefault(match.group(1), fields)
    return structs


def type_width(
    raw: str,
    aliases: dict[str, set[str]],
    structs: dict[str, list[str]],
    seen: frozenset[str] = frozenset(),
) -> int | None:
    ty = raw.strip()
    if not ty:
        return None
    if ty.startswith("*") or ty.startswith("&"):
        return 4
    if ty.startswith("Option<") and ty.endswith(">"):
        inner = ty[len("Option<") : -1].strip()
        if inner.startswith(("*", "&", "unsafe extern ")) or " fn(" in inner:
            return 4
        return None
    if ty.startswith("unsafe extern ") or " fn(" in ty:
        return 4
    compact = re.sub(r"\s+", "", ty)
    if compact in PRIMITIVE_BYTES:
        return PRIMITIVE_BYTES[compact]
    leaf = compact.split("::")[-1]
    if leaf in PRIMITIVE_BYTES:
        return PRIMITIVE_BYTES[leaf]
    if leaf in seen:
        return None
    targets = aliases.get(leaf, set())
    if len(targets) == 1:
        return type_width(next(iter(targets)), aliases, structs, seen | {leaf})
    if not targets:
        fields = structs.get(leaf, [])
        if len(fields) == 1:
            return type_width(fields[0], aliases, structs, seen | {leaf})
    return None


def arg_width(arg: str, aliases: dict[str, set[str]], structs: dict[str, list[str]]) -> int | None:
    if ":" not in arg:
        return None
    _, ty = arg.split(":", 1)
    return type_width(ty, aliases, structs)


def arg_stack_width(arg: str, aliases: dict[str, set[str]], structs: dict[str, list[str]]) -> int | None:
    """Return the x86 stack slot size for one parameter.

    Win32 x86 callers reserve at least one 4-byte stack slot for every argument,
    including BYTE/WORD parameters. Larger by-value values occupy enough whole
    slots to hold their resolved size.
    """
    width = arg_width(arg, aliases, structs)
    if width is None:
        return None
    return (width + 3) & ~3


def prototype_cleanup_bytes(
    abi: str,
    args: list[str],
    aliases: dict[str, set[str]],
    structs: dict[str, list[str]],
) -> int | None:
    if abi == "C":
        return 0
    widths = [arg_stack_width(arg, aliases, structs) for arg in args]
    if all(width is not None for width in widths):
        return sum(width for width in widths if width is not None)
    return None


def canonical_group(dll: str) -> str:
    if dll.startswith(("api-ms-win-core-", "api-ms-win-eventing-")):
        return "kernel32"
    if dll.startswith("api-ms-win-crt-") or dll.startswith(("msvcr", "vcruntime")) or dll in {
        "msvcrt",
        "ucrtbase",
        "ucrtbased",
    }:
        return "crt"
    if dll.startswith("msvcp"):
        return "msvcp"
    if dll == "api-ms-win-security-base-l1-1-0":
        return "kernel32"
    return KNOWN_GROUPS.get(dll, dll)


def import_inventory(exe: Path, analyzer: Path) -> tuple[list[tuple[str, str]], str, dict[str, object]]:
    result = subprocess.run(
        [str(analyzer), "analyze", str(exe), "--dir", str(exe.parent), "--json"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    report = json.loads(result.stdout)
    imports: list[tuple[str, str]] = []
    implemented: set[tuple[str, str]] = set()
    missing: set[tuple[str, str]] = set()
    unclassified: set[tuple[str, str]] = set()
    for dll in report.get("imports", []):
        library = Path(dll["dll"]).name.lower().removesuffix(".dll")
        implemented_names = set(dll.get("implemented", []))
        missing_names = set(dll.get("missing", []))
        all_names = set(dll.get("symbols", [])) | implemented_names | missing_names
        implemented.update((library, name) for name in implemented_names)
        missing.update((library, name) for name in missing_names)
        unclassified.update((library, name) for name in all_names - implemented_names - missing_names)
        for name in all_names:
            imports.append((library, name))
    coverage = {
        "counts": {
            "implemented": len(implemented),
            "missing": len(missing),
            "unclassified": len(unclassified),
        },
        "implemented": [{"dll": dll, "name": name} for dll, name in sorted(implemented)],
        "missing": [{"dll": dll, "name": name} for dll, name in sorted(missing)],
        "unclassified": [{"dll": dll, "name": name} for dll, name in sorted(unclassified)],
    }
    return sorted(set(imports)), hashlib.sha256(exe.read_bytes()).hexdigest(), coverage


def rust_table(entries: list[tuple[str, str, int]], fingerprints: list[tuple[str, str]]) -> str:
    header = [
        "// @generated by scripts/generate_win32_abi.py; do not edit by hand.",
        "// This file contains ABI cleanup metadata only; it contains no HLE behavior.",
        "// Inputs:",
    ]
    header += [f"//   {path} sha256={digest}" for path, digest in fingerprints]
    header += ["", "pub const API_CLEANUP: &[(&str, &str, u64)] = &[",]
    header += [f'    ("{group}", "{name}", {size}),' for group, name, size in entries]
    header += ["];", "", "pub fn cleanup_bytes(group: &str, name: &str) -> Option<u64> {", "    API_CLEANUP", "        .iter()", "        .find_map(|(dll, api, size)| (*dll == group && *api == name).then_some(*size))", "}", ""]
    return "\n".join(header)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", nargs="+", type=Path, help="PE32 executable(s) whose imports are in scope")
    parser.add_argument("--analyzer", type=Path, default=Path("target/release/anywasm.exe"))
    parser.add_argument("--windows-sys", help="windows-sys crate src directory (auto-detected by default)")
    parser.add_argument("--out", type=Path, default=Path("crates/anywasm-rt/src/x86_abi_generated.rs"))
    parser.add_argument("--report", type=Path, default=Path("target/x86-abi-audit.json"))
    args = parser.parse_args()

    src = find_windows_sys(args.windows_sys)
    aliases = aliases_in(src)
    structs = structs_in(src)
    prototypes: dict[tuple[str, str], set[tuple[str, int | None, str]]] = defaultdict(set)
    for record in macro_records(src):
        if record["abi"] not in ("system", "C"):
            continue
        size = prototype_cleanup_bytes(record["abi"], record["args"], aliases, structs)
        key = (record["dll"], record["name"])
        prototypes[key].add((record["abi"], size, record["source"]))

    imports: set[tuple[str, str]] = set()
    fingerprints = []
    target_coverage = []
    for exe in args.exe:
        resolved_exe = exe.resolve()
        found, digest, coverage = import_inventory(resolved_exe, args.analyzer.resolve())
        imports.update(found)
        fingerprints.append((str(resolved_exe), digest))
        target_coverage.append({"path": str(resolved_exe), **coverage})

    generated: dict[tuple[str, str], int] = {}
    unresolved = []
    for dll, name in sorted(imports):
        matches = prototypes.get((dll, name), set())
        group = canonical_group(dll)
        variants = {(abi, size) for abi, size, _ in matches}
        if len(variants) == 1:
            abi, size = next(iter(variants))
            source = sorted(source for _, _, source in matches)[0]
            if size is not None:
                generated[(group, name)] = size
                continue
            unresolved.append({"dll": dll, "name": name, "group": group, "reason": "windows-sys parameter shape is unresolved; check or add reviewed ABI metadata", "source": source})
        elif len(variants) > 1:
            unresolved.append({"dll": dll, "name": name, "group": group, "reason": "conflicting windows-sys prototypes; check or add reviewed ABI metadata", "prototypes": sorted([{"abi": abi, "stack_bytes": size, "source": source} for abi, size, source in matches], key=lambda item: (item["abi"], str(item["stack_bytes"]), item["source"]))})
        else:
            unresolved.append({"dll": dll, "name": name, "group": group, "reason": "no windows-sys prototype; check or add reviewed ABI metadata"})

    entries = sorted((group, name, size) for (group, name), size in generated.items())
    report = {
        "schemaVersion": 1,
        "windowsSysSource": str(src.resolve()),
        "targets": [{"path": path, "sha256": digest} for path, digest in fingerprints],
        "runtimeHandlerCoverage": target_coverage,
        "generated": [{"group": g, "name": n, "stackBytes": b} for g, n, b in entries],
        "prototypeUnresolved": unresolved,
        "counts": {"imports": len(imports), "generated": len(entries), "prototypeUnresolved": len(unresolved)},
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(rust_table(entries, fingerprints), encoding="utf-8", newline="\n")
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(report["counts"], sort_keys=True))
    print(f"wrote {args.out} and {args.report}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
