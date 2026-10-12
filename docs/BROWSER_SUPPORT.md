# Browser support evidence

This matrix records exercised behavior, not a compatibility promise. The Chromium browser suite is
`test/browser/run.mjs`; it launches headless Chromium through Playwright with
`--enable-unsafe-webgpu` and `--enable-features=Vulkan`. `CHROMIUM` can select the executable;
otherwise the script expects a Chromium under `/opt/pw-browsers`. The cross-engine smoke check is
`test/browser/cross-engine.mjs`; `CHROMIUM`, `FIREFOX`, and `WEBKIT` can select executables, and
`BROWSER` can select one engine.

An earlier local run on 2026-10-11: Chrome `154.0.8037.98`, selected with `CHROMIUM` on Windows
x64; `node test/browser/run.mjs` passed all checks. WebGPU reported an NVIDIA adapter, zero
validation errors, and successful triangle/textured back-buffer checks. This was still a headless
`?test=1` run, so it does not establish visible-canvas pixel output.

Fresh local cross-engine run on 2026-10-11: Firefox `141.0` and Playwright WebKit `18.2`, on Windows
x64. The hello fixture exited with code 0 in both. Firefox completed the D3D11 triangle fixture in
three frames. WebKit reported no WebGPU and no `transferControlToOffscreen`; the triangle stopped
with a clear `Graphics unavailable: OffscreenCanvas worker transfer is unsupported` status. A
synthetic cache-pressure probe compiled 300 unique emitted modules in Node, Firefox, and WebKit,
then verified that evicted modules recompile and retained modules execute from cache. The WebKit
result verifies explicit failure handling, not rendering support or native Safari behavior. The
Firefox process-exit result did not check pixels; a later readback found WebGPU validation errors and
no valid triangle output.

Fresh CSP run on 2026-10-11: Chromium `151.0.7922.34`, Firefox `141.0`, and Playwright WebKit `18.2`
loaded the page and same-origin worker with `default-src 'self'; script-src 'self'
'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; worker-src 'self'; connect-src 'self'`.
The hello worker exited successfully, and the emitted-module cache-pressure probe compiled and ran
without fallback in all three engines. This verifies that policy's requirements; it is not a general
CSP deployment guide.

Repeat run on 2026-10-11 using the currently installed Windows x64 browsers: Chromium
`151.0.7922.34`, Firefox `140.0.2`, and Playwright WebKit `18.2`. `node test/browser/run.mjs`
passed all checks, including visible triangle screenshot pixels and D3D11 textured back-buffer
pixels. `node test/browser/cross-engine.mjs` passed its core and CSP assertions in all three
engines. Firefox reported WebGPU unavailable and returned the expected explicit graphics-unavailable
status; WebKit reported WebGPU and OffscreenCanvas worker transfer unavailable and returned its
expected explicit status. This differs from the earlier Firefox `141.0` run, which completed the
triangle; the current results are specific to the installed browser builds and platform.

An earlier cross-engine run on 2026-10-12 found three Firefox WebGPU validation errors, beginning
with an unset bind group at index 1; its back buffer and visible canvas were black. The executor now
binds cached empty groups for unused pipeline slots, and the cross-engine check requires clean
validation plus matching pixels. A fresh run on 2026-10-12 used Chromium `154.0.8037.98`, Firefox
`141.0`, and Playwright WebKit `18.2`, explicitly selected through `CHROMIUM`, `FIREFOX`, and
`WEBKIT`. `node test/browser/cross-engine.mjs` passed its core and same-origin CSP assertions in all
three engines. Chromium and Firefox reported WebGPU and OffscreenCanvas worker transfer, exited the
hello and D3D11 triangle fixtures successfully (three frames), and passed the emitted-module
cache-pressure probe. Both passed 640x480 back-buffer checks and visible-canvas checks at 640x453;
each reported clear pixel `[0,0,51,255]`, back-buffer center `[85,86,85,255]`, visible-canvas center
`[85,85,84,255]`, and no WebGPU validation errors. The D3D11 fixture preserved R32_UINT/SINT words
with 8-byte rows, R32G32_FLOAT values and R32G32_UINT/SINT words with 16-byte rows, and
RGBA32_FLOAT/UINT/SINT rows with 32-byte pitches. R16 UNORM/SNORM/UINT/SINT readbacks used 4-byte
rows; RG16_FLOAT and the RG16 integer/normalized formats used 8-byte rows; RGBA16_FLOAT and the
RGBA16 integer/normalized formats used 16-byte rows. R8/RG8/RGBA8 SNORM/UINT/SINT and A8_UNORM also
preserved their 2-, 4-, and 8-byte rows. Both browsers preserved these bytes and the BGRA8_SRGB data.
R10G10B10A2_UINT and R11G11B10_FLOAT packed words also round-tripped with 8-byte rows.
WebKit exited hello, passed cache
pressure, and returned the expected explicit graphics-unavailable status because WebGPU and
OffscreenCanvas worker transfer are unavailable.
These results are specific to the installed browser builds and platform.

Fresh Chromium suite on 2026-10-12: Chrome `154.0.8037.98`, selected through `CHROMIUM` on Windows
x64. `node test/browser/run.mjs` passed the worker, emitted-module, input, D3D11 triangle, textured,
and compute checks. The `?test=1` triangle back-buffer readback matched barycentric samples at 216
pixels (worst error 1/255); a separate visible-canvas screenshot verified the clear colour and
interpolated centre pixel. The triangle fixture preserved two-row R32G32_FLOAT values at a 16-byte
pitch; R32G32_UINT/SINT packed words; RGBA32_FLOAT/UINT/SINT rows at a 32-byte pitch; the R16, RG16,
and RGBA16 normalized and integer staging patterns with 4-, 8-, and 16-byte row pitches; and encoded
bytes through RGBA8_SRGB and BGRA8_SRGB staging. R8/RG8/RGBA8 SNORM/UINT/SINT plus A8_UNORM
preserved 2-, 4-, and 8-byte rows; R10G10B10A2_UINT and R11G11B10_FLOAT preserved packed words.
The suite reported no WebGPU validation errors. The textured fixture
passed 18 indexed draws and its tested depth, blend,
texture, and copy cases. A 4x4 R32_FLOAT dispatch read back all 16 expected UAV values and the
following pixel shader sampled that result. The run reported no WebGPU validation errors or page
errors. The D3D9 fixed-function fixture also passed pixel checks for stage-0 SELECTARG1/SELECTARG2,
MODULATE/MODULATE2X, ADD/SUBTRACT, ADDSMOOTH, alpha MODULATE2X/MODULATE4X/ADD/SUBTRACT/ADDSMOOTH,
signed-add and modulate-add operations, DOTPRODUCT3, and texture-stage blending with diffuse,
texture, factor, premultiplied-texture, and current-alpha inputs for color and alpha. Alpha testing
and render-target blending are also checked. It does not add target-game or
non-Chromium pixel coverage beyond the D3D11 triangle.

| Browser or feature | Evidence and current status |
|---|---|
| Chromium, headless | **Partial, exercised by the browser suite.** It drives the real page and worker; checks load progress and accessible unimplemented-import diagnostics; verifies keyboard input, CSS-to-guest mouse scaling, `TrackMouseEvent(TME_LEAVE)`/`WM_MOUSELEAVE`, and compares opt-in emitted execution with cached execution. The selected maze replay checks W/S/A/D, cursor-aimed movement in all four directions, and a flashlight response for one seed. |
| Content Security Policy | **Partial, exercised in Chromium, Firefox, and WebKit.** A same-origin policy allowing `wasm-unsafe-eval` permits the page and worker to run hello and the emitted-module cache-pressure probe. Other deployment policies and directives remain untested. |
| WebGPU through Chromium | **Partial, exercised by the browser suite.** D3D11 triangle and textured fixtures are checked by reading back the GPU back buffer in `?test=1` mode. The selected Diligent maze replay also reads its 128x128 R32_FLOAT SDF UAV in test mode. A separate non-test-mode run captures the visible 640x480 triangle canvas and checks its clear colour and interpolated centre pixel. Other rendering paths and browser engines remain unverified. |
| Firefox | **Partial, pixel verified for the D3D11 triangle on 141.0.** The cross-engine check verifies clean WebGPU validation, 640x480 back-buffer clear/center pixels, and visible-canvas clear/center pixels. A separate Windows Firefox 140.0.2 run exited hello but reported WebGPU unavailable and stopped graphics with an explicit status. Other graphics paths and browser builds remain unverified. |
| Safari / WebKit | **Partial, smoke tested with Playwright WebKit 18.2.** Hello exits successfully. The installed engine lacks WebGPU and OffscreenCanvas worker transfer; graphics programs stop with an explicit limitation. This does not establish native Safari behavior. |
| Large-game memory, `memory64`, asset streaming | **Unmeasured / open.** The build target is `wasm32-unknown-unknown`. The browser buffers the complete `.awb` as an `ArrayBuffer`, copies it into WASM linear memory, then `Bundle::from_bytes` copies each file into the in-memory VFS. No 4 GB pressure test, memory64 build, or large-asset streaming test has run; this is a source-level assessment, not a browser measurement. |

The checked-in assertions establish their scope; they do not test the GLFW Demo or a real game.
Firefox graphics coverage is limited to the D3D11 triangle; WebKit only verifies the explicit
graphics-unavailable path. No full cross-browser support claim
should be made based on these runs. See [`CLAUDE.md`](../CLAUDE.md) for the project checklist and
[`STATUS.md`](STATUS.md) for implementation limits.
