# Third-party components

| Component | Where | License | Notes |
|---|---|---|---|
| libvkd3d-shader 2.1 (WASM) | `web/shaders/vkd3d-shader.{js,wasm}` | LGPL-2.1-or-later | HLSL->DXBC and DXBC->SPIR-V. Shipped as a separate WASM module; source tarball in `web/shaders/licenses/vkd3d-2.1.tar.xz`. Built and bridged by the WineBrowser project (MIT bridge code, `web/shaders/vkd3d-bridge.c.txt`). |
| Naga 30.0.1 (WASM) | `web/shaders/naga/` | MIT OR Apache-2.0 | SPIR-V->WGSL. Per-crate licenses in `web/shaders/licenses/naga/`. |
| mingw-w64 headers | used at generation time by `tools/gen_vtables.py` | ZPL/public domain | Only COM method order/IIDs are derived (`crates/anywasm-rt/src/hle/vtables.rs`). |

Both WASM modules were taken unmodified from <https://github.com/evangit2/winebrowser> (MIT), which
pins and documents their provenance. No AnyPS5 or Canary code is used; AnyPS5 informed the design only.
