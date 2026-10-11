# Third-party components

| Component | Where | License | Notes |
|---|---|---|---|
| libvkd3d-shader 2.1 (WASM) | `web/shaders/vkd3d-shader.{js,wasm}` | LGPL-2.1-or-later | HLSL->DXBC and DXBC->SPIR-V. Shipped as a separate WASM module; source tarball in `web/shaders/licenses/vkd3d-2.1.tar.xz`. Built and bridged by the WineBrowser project (MIT bridge code, `web/shaders/vkd3d-bridge.c.txt`). |
| Naga 30.0.1 (WASM) | `web/shaders/naga/` | MIT OR Apache-2.0 | SPIR-V->WGSL. Per-crate licenses in `web/shaders/licenses/naga/`. |
| Diligent Engine GLFWDemo shader fixture | `fixtures/diligent_glfwdemo_sdf.dxbc` | Apache-2.0 | Test-only compiled `GenerateSDF.hlsl`; source revision and compiler macros are recorded in `fixtures/README.md`, with license text in `fixtures/licenses/`. |
| mingw-w64 headers | used at generation time by `tools/gen_vtables.py` | ZPL/public domain | Only COM method order/IIDs are derived (`crates/anywasm-rt/src/hle/vtables.rs`). |

Both WASM modules were taken unmodified from <https://github.com/evangit2/winebrowser> (MIT), which
pins and documents their provenance. No AnyPS5 or Canary code is used; AnyPS5 informed the design only.

## Redistribution review

This is a technical inventory, not a legal opinion. The repository currently has no packaged-app
release flow, so these checks must be repeated against the exact files in any future package.

- The vkd3d-shader module is distributed as a separate `.wasm` file with its JavaScript wrapper.
  The vkd3d 2.1 source archive, license text, copyright notice, and the bridge license are kept under
  `web/shaders/licenses/`. The upstream vkd3d project identifies the shader library as LGPL-2.1-or-later.
- For redistribution, retain a prominent vkd3d notice and the full LGPL text. Make the corresponding
  source for the exact library build available, including any changes; verify that the checked-in
  source archive matches the shipped WASM bits and build configuration. LGPL-2.1 section 6 also
  specifies modification and reverse-engineering permissions for a work using the library, and
  relinking materials in cases where the distributed work is linked with the library. Whether a
  separately loaded WebAssembly module and the current JS interface meet those conditions for a
  particular packaged app needs legal review; this repository does not claim that determination.
- Naga's resolved crate and dependency notices are retained in
  `web/shaders/licenses/naga-THIRD_PARTY_NOTICES.md` and its linked license files. Keep those notices
  with any redistributed Naga artifacts.
- Before shipping, verify that the package includes the notices and exact matching sources, and that
  users can replace or rebuild the vkd3d module in the shipped application. Obtain legal review of
  the combined AnyWASM, bridge, vkd3d, Naga, and package terms.

References: [LGPL-2.1 text](https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html),
[vkd3d upstream](https://github.com/ValveSoftware/vkd3d).
