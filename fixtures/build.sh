#!/usr/bin/env bash
# Rebuild the PE fixtures with clang + lld-link (no Windows SDK needed).
set -euo pipefail
cd "$(dirname "$0")"
T=x86_64-pc-windows-msvc
for d in kernel32 user32 gdi32 shell32 shlwapi d3d11; do
  llvm-dlltool -m i386:x86-64 -d $d.def -l $d.lib
done
llvm-dlltool -m i386:x86-64 -d ucrtstdio.def -l ucrtstdio.lib
build() {  # name, subsystem, extra libs...
  local n=$1 sub=$2; shift 2
  clang --target=$T -O2 -fno-stack-protector -fno-builtin -ffreestanding -mno-stack-arg-probe -c $n.c -o $n.obj
  lld-link /nologo /subsystem:$sub /entry:mainCRTStartup /nodefaultlib /dynamicbase /base:0x140000000 /out:$n.exe $n.obj "$@"
}
build hello console kernel32.lib
build multibyte_test console kernel32.lib
build lcmap_test console kernel32.lib
build verconditionmask_test console kernel32.lib
build pathfileexists_test console kernel32.lib shlwapi.lib
build thread_test console kernel32.lib
build thread_sync_test console kernel32.lib
# The freestanding clang/lld path does not provide the CRT object that emits a PE TLS directory.
# Build this one with a Visual Studio developer environment so libcmt's tlssup.obj emits it.
if command -v cl.exe >/dev/null && command -v link.exe >/dev/null && [[ -n "${VCToolsInstallDir:-}" ]]; then
  cl.exe /nologo /c /O2 /GS- /Oi- /Zl /Fo:static_tls_thread_test.obj static_tls_thread_test.c
  link.exe /nologo /subsystem:console /entry:mainCRTStartup /nodefaultlib /include:_tls_used \
    /dynamicbase /base:0x140000000 /out:static_tls_thread_test.exe \
    static_tls_thread_test.obj kernel32.lib "${VCToolsInstallDir}/lib/x64/libcmt.lib"
else
  echo "Skipping static_tls_thread_test.exe rebuild: requires a Visual Studio developer environment" >&2
fi
build slist_test console kernel32.lib
build compute console kernel32.lib
build reloc console kernel32.lib
build d3dprobe windows kernel32.lib user32.lib d3d11.lib
build input_test windows kernel32.lib user32.lib
build capture_test windows kernel32.lib user32.lib
build iswindowunicode_test windows kernel32.lib user32.lib
build getdlgitem_test windows kernel32.lib user32.lib
build device_notification_test windows kernel32.lib user32.lib
build enumdisplaydevices_test windows kernel32.lib user32.lib
build enumdisplaysettings_test windows kernel32.lib user32.lib
build enumdisplaymonitors_test console kernel32.lib user32.lib
build windowprops_test windows kernel32.lib user32.lib
build rect_test console kernel32.lib user32.lib
build dragaccept_test windows kernel32.lib user32.lib shell32.lib
build gdiprobe_test console kernel32.lib gdi32.lib
build mapvirtualkey_test console kernel32.lib user32.lib
build tounicode_test console kernel32.lib user32.lib
build wfopen_s_test console kernel32.lib ucrtstdio.lib
build cwd_test console kernel32.lib ucrtstdio.lib
build cwd_bundle_test console kernel32.lib
# Real CRT-linked program (needs gcc-mingw-w64-x86-64); output was cross-checked against a native build.
if command -v x86_64-w64-mingw32-gcc >/dev/null; then
  x86_64-w64-mingw32-gcc -O2 -o crt_test.exe crt_test.c
  x86_64-w64-mingw32-gcc -O2 -o unimpl.exe unimpl.c
  x86_64-w64-mingw32-gcc -O2 -o d3d11_triangle.exe d3d11_triangle.c -ld3d11 -ldxgi -ldxguid -luser32  # shaders: node fixtures/gen_d3d_shaders.mjs
  x86_64-w64-mingw32-gcc -O2 -o d3d11_textured.exe d3d11_textured.c -ld3d11 -ldxgi -ldxguid -luser32
fi
rm -f *.obj *.lib
ls -l *.exe
