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
rm -f *.obj *.lib
ls -l *.exe
