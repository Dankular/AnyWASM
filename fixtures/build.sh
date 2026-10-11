#!/usr/bin/env bash
# Rebuild the PE fixtures with clang + lld-link (no Windows SDK needed).
set -euo pipefail
cd "$(dirname "$0")"
T=x86_64-pc-windows-msvc
for d in kernel32 user32 d3d11; do
  llvm-dlltool -m i386:x86-64 -d $d.def -l $d.lib
done
build() {  # name, subsystem, extra libs...
  local n=$1 sub=$2; shift 2
  clang --target=$T -O2 -fno-stack-protector -fno-builtin -ffreestanding -mno-stack-arg-probe -c $n.c -o $n.obj
  lld-link /nologo /subsystem:$sub /entry:mainCRTStartup /nodefaultlib /dynamicbase /base:0x140000000 /out:$n.exe $n.obj "$@"
}
build hello console kernel32.lib
build compute console kernel32.lib
build reloc console kernel32.lib
build d3dprobe windows kernel32.lib user32.lib d3d11.lib
# Real CRT-linked program (needs gcc-mingw-w64-x86-64); output was cross-checked against a native build.
if command -v x86_64-w64-mingw32-gcc >/dev/null; then
  x86_64-w64-mingw32-gcc -O2 -o crt_test.exe crt_test.c
  x86_64-w64-mingw32-gcc -O2 -o unimpl.exe unimpl.c
  x86_64-w64-mingw32-gcc -O2 -o d3d11_triangle.exe d3d11_triangle.c -ld3d11 -ldxgi -ldxguid -luser32  # shaders: node fixtures/gen_d3d_shaders.mjs
  x86_64-w64-mingw32-gcc -O2 -o d3d11_textured.exe d3d11_textured.c -ld3d11 -ldxgi -ldxguid -luser32
fi
rm -f *.obj *.lib
ls -l *.exe
