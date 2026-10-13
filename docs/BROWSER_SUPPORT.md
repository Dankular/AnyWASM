# Browser support evidence

This matrix records exercised behavior, not a compatibility promise. The Chromium browser suite is
`test/browser/run.mjs`; it launches headless Chromium through Playwright with
`--enable-unsafe-webgpu` and `--enable-features=Vulkan`. `CHROMIUM` can select the executable;
otherwise the script expects a Chromium under `/opt/pw-browsers`. The cross-engine smoke check is
`test/browser/cross-engine.mjs`; `CHROMIUM`, `FIREFOX`, and `WEBKIT` can select executables, and
`BROWSER` can select one engine.

Focused input fixture repeat on 2026-10-13: Chrome `155.0.8059.40` on Windows x64. The 256x192 CSS
content area maps the test point to guest client coordinate `(520,404)`; the guest reports
`TrackMouseEvent(TME_LEAVE)` success, receives `WM_MOUSELEAVE`, and exits with code 0. The fixture
aligns the canvas to integer CSS pixels so browser pointer rounding does not shift the target point.

Focused cube-array regression on 2026-10-13: Playwright Chromium `151.0.7922.34` on Windows x64.
The synthetic D3D11 fixture selected two cubes starting at face 6, sampled both cube indices,
checked the linear +X/+Y edge blend, and read the selected view dimensions as 4x4 with two cubes and
one mip. The fixture reported one draw, zero WebGPU validation errors, and pixels
`[0,255,0,255]`, `[0,0,255,255]`, `[0,127,127,255]`, and `[255,255,32,255]`. This is API fixture
evidence, not a selected-target compatibility result.

The cube-array query fixture was extended on 2026-10-13 in Chrome `155.0.8059.40` to use two mip
levels. `GetDimensions(0)` returns 4x4 and `GetDimensions(1)` returns 2x2; both report two cubes
and two levels. The captured normalized pixels were `[255,255,32,255]` and `[128,128,32,255]`.
Cube samples, the +X/+Y seam, and mip queries completed with one draw and no WebGPU errors.

Focused depth cube-array comparison regression on 2026-10-13: Chrome `155.0.8059.40` on Windows x64.
The synthetic D3D11 fixture covers implicit `SampleCmp` and `SampleCmpLevelZero` on selected two-cube
D32_FLOAT, D16_UNORM, D24_UNORM_S8, and D32_FLOAT_S8X24 SRVs starting at face 6. D24 uses R24G8_TYPELESS
with D24_UNORM_S8 DSVs; D32S8 uses R32G8X24_TYPELESS with D32_FLOAT_S8X24 DSVs and requires Chrome's
`depth32float-stencil8` feature. Both packed formats use depth-only SRVs. Separate DSVs clear the cubes
to 0.25 and 0.75; LESS against 0.5 returns false for cube 0 and true for cube 1 in all four formats
and both sampling methods, encoded as `[0,0,0,255]` and `[255,255,255,255]`, with one draw and zero
WebGPU errors. This API fixture also verifies a one-slice DSV view selects its nonzero array layer; it
is not a selected-target compatibility result.

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
following pixel shader sampled that result. Texture UAV dispatch/readback also covers every format
advertised by `D3D11_FEATURE_FORMAT_SUPPORT`: R32 and RG32 float/uint/sint, RGBA8 unorm/snorm/uint/sint,
RGBA16 float/uint/sint, and RGBA32 float/uint/sint. The run reported no WebGPU validation errors or
page errors. The D3D9 fixed-function fixture also passed pixel checks for stage-0 SELECTARG1/SELECTARG2,
MODULATE/MODULATE2X, ADD/SUBTRACT, ADDSMOOTH, alpha MODULATE2X/MODULATE4X/ADD/SUBTRACT/ADDSMOOTH,
signed-add and modulate-add operations, DOTPRODUCT3, MULTIPLYADD/LERP with COLORARG0/ALPHAARG0,
and texture-stage blending with diffuse,
texture, factor, premultiplied-texture, and current-alpha inputs for color and alpha. Alpha testing
and render-target blending are also checked. It does not add target-game or
non-Chromium pixel coverage beyond the D3D11 triangle.

On 2026-10-12, `node test/browser/run.mjs` was rerun on Windows x64 with Chrome `151.0.7922.34`;
the full suite passed with no WebGPU validation errors or page errors. Its 4x MSAA D3D11 regressions
pixel-check resolves for RGBA8, BGRA8, RGBA16_FLOAT, RGBA8_SRGB, BGRA8_SRGB, R10G10B10A2_UNORM,
and R11G11B10_FLOAT; the packed 10-bit normalized and unsigned-float channels match the fractional
shader output. Other cases resolve half-alpha red
over a blue clear to verify alpha-to-coverage, and verify that sample mask `0x5` preserves two of
four samples. A two-target pixel check also verifies independent blend
descriptors by adding red to target 0 while target 1 overwrites its clear colour.
The D3D11 BORDER gather regression covers 2D and 2D-array RGBA8 taps at an edge and outside the
image, an explicit nonzero gather offset, D32_FLOAT comparison gathers, and R8_UNORM alpha-channel
defaults.
The suite also checks 4x D24S8 depth/stencil attachments: a near green draw occludes a later far red
draw, and the 4x RGBA8 color target resolves to the expected green pixel. Rust HLE tests verify
4x quality-level reports for D32_FLOAT, D24S8, and D16, reject 8x, and accept typeless D24S8
multisampled depth-resource creation.
The blend fixture also checks all four D3D11 `SRC1_*` color and alpha factors using the
second pixel-shader output, including inverse factors and a fractional source alpha. A channel
write-mask check writes red and blue while preserving the cleared green and alpha channels. A
separate-alpha check uses `ONE/ZERO/ADD` for RGB and `ONE/ONE/SUBTRACT` for alpha. A
depth write-mask check confirms `ZERO` lets a farther later draw pass while `ALL` keeps the nearer
fragment visible; the depth-disabled control also renders its later draw. Stencil coverage includes
a partial `0x0f` write that preserves the upper nibble and a `0x0f` read mask that compares only the
lower nibble. Texture dispatch/readback checks every typed UAV format currently advertised by the
HLE: R32 and RG32 float/uint/sint, RGBA8 unorm/snorm/uint/sint, RGBA16 float/uint/sint, and RGBA32
float/uint/sint.

A follow-up full Chromium suite on 2026-10-12 also passed visible swapchain checks at 32x24 and
96x64 after `ResizeBuffers`, with backing and CSS dimensions updated, expected clear pixels at both
canvas edges, two presents, and no WebGPU validation errors. A non-test-mode D3D11 triangle run at
DPR 2 kept its guest backing store at 640x480, presented three frames, and matched the expected
clear and interpolated triangle pixels in the 1280-pixel-wide screenshot with no page or WebGPU
errors. When `depth32float-stencil8` is available, D32_FLOAT_S8X24 readback also preserves the
little-endian depth value, stencil byte, and zeroed 24-bit padding for all four texels in a 2x2
texture. D24_UNORM_S8 readback combines a shader-sampled, 24-bit-quantized depth aspect with a
stencil-only copy; a 2x2 clear checks packed bytes `[0,0,96,90]` for every texel. B5G6R5_UNORM,
B5G5R5A1_UNORM, and B4G4R4A4_UNORM use an RGBA16Float backing and preserve normalized channels
and raw packed texels across odd-width, padded 3x2 rows.

| Browser or feature | Evidence and current status |
|---|---|
| Chromium, headless | **Partial, exercised by the browser suite.** It drives the real page and worker; checks load progress and accessible unimplemented-import diagnostics; verifies keyboard input, CSS-to-guest mouse scaling, `TrackMouseEvent(TME_LEAVE)`/`WM_MOUSELEAVE`, and compares opt-in emitted execution with cached execution. The selected maze replay checks W/S/A/D, cursor-aimed movement in all four directions, and a flashlight response for one seed. |
| Content Security Policy | **Partial, exercised in Chromium, Firefox, and WebKit.** A same-origin policy allowing `wasm-unsafe-eval` permits the page and worker to run hello and the emitted-module cache-pressure probe. Other deployment policies and directives remain untested. |
| WebGPU through Chromium | **Partial, exercised by the browser suite.** D3D11 triangle and textured fixtures are checked by reading back the GPU back buffer in `?test=1` mode. The selected Diligent maze replay also reads its 128x128 R32_FLOAT SDF UAV in test mode. Staging checks cover D32_FLOAT_S8X24 when the optional format is available, D24_UNORM_S8, and three packed 16-bit BGR formats. Non-test-mode runs check visible triangle pixels at 640x480 and DPR 2, plus visible swapchain pixels and backing/CSS sizes before and after resize from 32x24 to 96x64. Other rendering paths and browser engines remain unverified. |
| Firefox | **Partial, pixel verified for the D3D11 triangle on 141.0.** The cross-engine check verifies clean WebGPU validation, 640x480 back-buffer clear/center pixels, and visible-canvas clear/center pixels. A separate Windows Firefox 140.0.2 run exited hello but reported WebGPU unavailable and stopped graphics with an explicit status. Other graphics paths and browser builds remain unverified. |
| Safari / WebKit | **Partial, smoke tested with Playwright WebKit 18.2.** Hello exits successfully. The installed engine lacks WebGPU and OffscreenCanvas worker transfer; graphics programs stop with an explicit limitation. This does not establish native Safari behavior. |
| Large-game memory, `memory64`, asset streaming | **Unmeasured / open.** The build target is `wasm32-unknown-unknown`. The browser buffers the complete `.awb` as an `ArrayBuffer`, copies it into WASM linear memory, then `Bundle::from_bytes` copies each file into the in-memory VFS. No 4 GB pressure test, memory64 build, or large-asset streaming test has run; this is a source-level assessment, not a browser measurement. |

The checked-in assertions establish their scope; they do not test the GLFW Demo or a real game.
Firefox graphics coverage is limited to the D3D11 triangle; WebKit only verifies the explicit
graphics-unavailable path. No full cross-browser support claim
should be made based on these runs. See [`CLAUDE.md`](../CLAUDE.md) for the project checklist and
[`STATUS.md`](STATUS.md) for implementation limits.
