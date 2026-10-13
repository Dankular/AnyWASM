# Graphics Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every remaining graphics item in `CLAUDE.md` section 3 and GitHub issue #3, with runtime or pixel evidence for each claimed capability, then push the verified commits.

**Architecture:** Keep the existing Rust HLE command stream and WebGPU executor as the primary path, adding API-specific layers only where a task requires them. Split work into independently testable D3D11 verification, compute/UAV, shader translation, presentation, D3D9, DXGI/D3D12, OpenGL, and Vulkan slices. Maintain a distinction between API implementation, target-grounded behavior, and cross-engine evidence.

**Tech Stack:** Rust HLE/runtime, JavaScript WebGPU executor, vkd3d-shader, Naga, Node browser fixtures, Playwright Chromium/Firefox/WebKit, native D3D11 capture tools.

**Spec:** `CLAUDE.md` section 3; `docs/STATUS.md`; `docs/TARGETS.md`; `docs/BROWSER_SUPPORT.md`; GitHub issue `https://github.com/Dankular/AnyWASM/issues/3` and its comments.

## Global Constraints

- Preserve existing guest-visible HRESULT/status behavior and report unsupported operations explicitly.
- Add a focused regression before each behavior change; use pixel/readback evidence for graphics output.
- Keep target-game claims separate from synthetic API fixture results.
- Stage and push only reviewed graphics files; preserve unrelated shared worktree changes.
- Update the graphics checklist and browser/target evidence after the implementation they describe passes.

## Review Focus

- WebGPU pipelines with sparse bind-group indices and resource-free groups must validate in Firefox and Chromium.
- Texture and buffer offsets, row pitches, subresource ranges, and format conversions must preserve edge texels and untouched destination data.
- Blend, depth, stencil, rasterizer, and alpha-test state combinations must retain independent channel semantics.
- Query, UAV, and staging paths must expose writes in command order and preserve type/format conversion.
- Present timing, canvas resizing, and HiDPI dimensions must agree between guest back buffers and visible canvases.

---

### Task 1: Reconcile current graphics evidence

**Files:** `CLAUDE.md`, `docs/STATUS.md`, `docs/BROWSER_SUPPORT.md`, `docs/TARGETS.md`

- [x] Compare each open checkbox with current source and test names; mark only evidence-backed work complete.
- [x] Carry the pushed D3D9 operation coverage into the current checklist without staging unrelated CPU/runtime edits from shared documentation files.
- [x] Preserve a list of remaining requirements in this plan and link each to its owning task below.

**Verification:** Search the four source-of-truth documents and confirm every open graphics bullet maps to Tasks 2–9.

### Task 2: Resolve Firefox WebGPU bind-group validation

**Files:** `web/gpu.js`, `test/browser/cross-engine.mjs`, `docs/BROWSER_SUPPORT.md`

- [x] Add a Firefox assertion for zero WebGPU validation errors and the expected 640x480 clear/triangle pixels.
- [x] Bind all pipeline groups used by the current render layout, including empty group layouts, without changing shader resource layouts.
- [x] Run the cross-engine fixture and record the engine versions and adapter results.

**Gate:** PASS. Firefox 141.0 and Chromium 151.0.7922.34 returned the expected 640x480 back-buffer clear `[0,0,51,255]` and center `[85,86,85,255]`, matching visible-canvas pixels and zero WebGPU validation errors. The test output records Firefox's 640x453 screenshot including canvas borders.

### Task 3: Finish D3D11 render-state and resource verification

**Files:** `test/browser/run.mjs`, `web/gpu.js`, relevant D3D11 HLE files, `CLAUDE.md`

#### Remaining verification inventory (2026-10-13)

| Area | Existing evidence | Still open |
|---|---|---|
| Sample dimensions and formats | `test/browser/run.mjs`: Texture1DArray, Texture2DArray, Texture3D, TextureCube, and TextureCubeArray paths; R8_UNORM samples cover Texture1DArray mip/layer ranges, Texture2DArray anisotropic edges, Texture3D BORDER footprints, and TextureCubeArray selection across two cubes in a view beginning at face 6; RG8_UNORM covers anisotropic Texture2DArray edges, Texture3D outside/edge BORDER samples, and TextureCubeArray selected-cube checks; R16_UNORM/SNORM/FLOAT samples cover Texture3D edge footprints and cube-array selection; cube-array color samples also use RGBA8, RGBA8_SRGB, RGBA16_FLOAT, R16_FLOAT, R10G10B10A2_UNORM, and R11G11B10_FLOAT; depth comparison cube arrays use D32_FLOAT, D16_UNORM, D24_UNORM_S8, and D32_FLOAT_S8X24. | More array/3D sampled formats and cube-array formats/dimensions remain open. `SampleCmpBias`/`SampleCmpGrad` are blocked by the bundled vkd3d compiler: E5005 under `ps_5_0` and E5033 (unknown profile) under `ps_6_0`/`ps_6_8`. |
| Sampler footprints | Point/linear address modes, 2D/array/3D BORDER edges, gather taps, comparison taps, 8x anisotropic 2D/array/comparison footprints, R8_UNORM and RG8_UNORM Texture2DArray Sample/SampleBias/SampleGrad edges, and 8x anisotropic BC4_UNORM Texture2D Sample/SampleBias/SampleGrad edges have pixel checks. | Other anisotropic footprints and sampled format combinations remain open. |
| Copies and mip generation | `CopyResource` covers texture, vertex/index, constant, structured, raw, and typed buffers; `CopySubresourceRegion` covers partial RG8_UNORM 2D boxes, a partial R16_UNORM/SNORM column copy with copied and untouched words preserved through staging readback, block-aligned BC1/BC2/BC3/BC4/BC5/BC6H/BC7 copies preserving their neighbouring blocks, other 2D boxes, a 3D box across two slices with untouched voxels, a 2D-array copy between mip/layer subresources, unaligned 11-byte buffer ranges, and overlapping same-buffer ranges; `UpdateSubresource`, 2D/3D `GenerateMips`, and the listed MSAA resolves have regressions. Its browser executor now reads the wire payload's six source-box coordinates before the trailing `hasBox` flag. | The tests are representative boundaries, not a full format × mip × array × region matrix; remaining combinations need named edge-preservation checks. |
| Resolve and multisample creation | Chromium checks 4x resolve pixels for RGBA8, BGRA8, RGBA16_FLOAT, RGBA8_SRGB, BGRA8_SRGB, R10G10B10A2_UNORM, and R11G11B10_FLOAT. The HLE table test checks 2x/4x/8x quality reporting for all seven color and three depth formats it advertises, plus representative unsupported formats. Paired HLE regressions verify RGBA8 sample-count reporting/creation consistency and 4x multisampled array creation. Chromium near/far depth pixels cover 4x D32_FLOAT, D24S8, and D16_UNORM attachments through RGBA8 color resolve; a two-layer RGBA8 test resolves only layer 1 and preserves layer 0. Incompatible DXGI format and mismatched extents are rejected while preserving destination clear pixels. | Other resolve edge combinations remain untested. |
| Render state | Pixel checks cover blend factors/operations, independent targets, masks, alpha-to-coverage, sample mask, all depth comparisons, selected depth-write behavior, all stencil comparisons/operations, culling, front-face, scissor, wireframe, and depth bias. A 4x D24S8 test combines alpha-to-coverage with a farther green draw and checks the resolved red/green sample result. | The full cross-product of render states and attachment/sample configurations is not covered. |
| Staging layouts | `test/browser/run.mjs` covers typed, normalized, packed, sRGB, depth/stencil, BC, odd-width rows, selected mip/array slices, and 3D row/depth pitches. R16_UNORM/SNORM upload words are retained through full-resource copies into staging; sampling uses R32_FLOAT backing when `float32-filterable` is available and half-float fallback otherwise. `Map(READ)` drains older host-visible GPU batches before its current HLE batch, preserving shared-worker command order. | Other formats and subresource layouts remain open. |

- [x] Enumerate the untested format/resolve combinations, copy classes, sampler/render-state combinations, and staging layouts from section 3.
- [ ] Add boundary-focused pixel/readback cases for each open behavior, including preservation outside copied/cleared regions.
- [ ] Implement only the failing HLE/executor behavior and rerun the affected browser checks plus the full Chromium suite.
- [x] Advertise and forward 4x multisample creation for R10G10B10A2_UNORM and R11G11B10_FLOAT; HLE quality/creation tests pass and the Chromium resolve fixture verifies both formats' pixels.
- [x] Forward D3D11 alpha-to-coverage into multisampled WebGPU pipelines; the 4x RGBA8 resolve regression verifies partial red/blue sample coverage from a half-alpha draw in Chromium.
- [x] Forward the `OMSetBlendState` sample mask into WebGPU multisample state and its pipeline cache key; the 4x RGBA8 resolve regression verifies that mask `0x5` preserves two of four samples.
- [x] Pixel-check 4x resolve output for RGBA8, BGRA8, RGBA16_FLOAT, RGBA8_SRGB, BGRA8_SRGB, R10G10B10A2_UNORM, and R11G11B10_FLOAT render targets; the packed normalized and unsigned-float resolve channels match the fractional shader output, and alpha-to-coverage and sample-mask resolves preserve the untouched blue background.
- [x] Table-test 2x/4x/8x quality reporting for the seven advertised color formats and three depth formats, and representative unsupported formats; accept typeless D24S8 4x depth-resource creation; pixel-check near/far depth testing with 4x D32_FLOAT, D24S8, and D16 attachments plus RGBA8 color resolves.
- [x] Verify the multisample quality query and `CreateTexture2D` agree for RGBA8: 2x/8x report zero quality levels and reject creation without emitting a resource, while the supported 4x path succeeds.
- [x] Verify an incompatible BGRA8 resolve request for an RGBA8 multisampled source leaves the RGBA8 destination's green clear pixel intact and produces no WebGPU validation error.
- [x] Verify a same-format resolve with 8x8 source and 4x4 destination preserves the destination's yellow clear pixels and produces no WebGPU validation error.
- [x] Accept 4x multisampled RGBA8 render-target arrays in the HLE and resolve only array layer 1 in Chromium; layer 0 retains its blue clear while layer 1 resolves green. The executor represents each layer with its own 2D multisampled WebGPU texture.
- [x] Report `D3D11_FORMAT_SUPPORT_DEPTH_STENCIL` only for the four typed DSV formats; a unit regression checks D32, D32S8, D24S8, and D16 support while RGBA16_FLOAT, RGBA8, and R8 omit the bit.
- [x] Combine 4x D24S8 depth testing with alpha-to-coverage: a near half-alpha red draw and farther green draw resolve to red/green samples with no WebGPU errors.
- [x] Combine 4x D24S8 depth testing with complementary `OMSetBlendState` sample masks (`0x5` then `0xA`); the resolve is `[128,128,0,255]` with no WebGPU errors.
- [x] Verify `IndependentBlendEnable` with two render targets: target 0 adds red to its blue clear while target 1 overwrites green with blue.
- [x] Implement D3D11 `SRC1_COLOR`, `INV_SRC1_COLOR`, `SRC1_ALPHA`, and `INV_SRC1_ALPHA` blend factors using the second pixel-shader output; browser pixels verify each factor.
- [x] Verify a D3D11 render-target write mask of `0x5` updates red/blue while preserving green/alpha from the clear.
- [x] Verify independent D3D11 RGB and alpha blend factors/operations with separate pixel expectations.
- [x] Verify D3D11 depth write mask `ZERO` allows a later farther draw through, while `ALL` preserves the nearer result.
- [x] Pixel-check all eight D3D11 depth comparisons with incoming depth below, equal to, and above the cleared reference value.
- [x] Verify stencil write mask `0x0f` preserves upper bits and stencil read mask `0x0f` compares only lower bits.
- [x] Verify D3D11 stencil fail and depth-fail paths for KEEP, ZERO, REPLACE, saturating increment/decrement, invert, wrapping increment/decrement, and masked replace; all boundary pixels pass in the full Chromium suite with no WebGPU validation errors.
- [x] Pixel-check D3D11 WRAP, MIRROR, CLAMP, and MIRROR_ONCE modes and D3D9 fixed-function MIRROR_ONCE; D3D11 BORDER verifies the configured color for point taps and linear footprints outside and straddling 2D RGBA8 edges through Sample, SampleLevel, SampleGrad, and SampleBias, plus 2D-array and 3D RGBA8 edges through SampleLevel. D32_FLOAT comparison BORDER verifies all eight comparison functions for fully outside point-filtered SampleCmpLevelZero taps and for edge-straddling and fully outside linearly filtered SampleCmp footprints. R8_UNORM, RG8_UNORM, A8_UNORM, B5G6R5_UNORM, BC1, and BC4 BORDER pixels verify view-format channel defaults and format-sensitive variant caching; BC2/BC3 edge samples verify explicit/interpolated alpha block values at 100 and 141; BC5 edge/outside pixels verify RG defaults at `[192, 96, 0, 255]` and `[255, 64, 0, 255]`; the A8_UNORM fully outside sample returns `[0, 0, 0, 64]`. 2D and 2D-array RGBA8 Gather plus D32_FLOAT GatherCmp verify per-tap border behavior at edge-straddling and fully outside footprints; RGBA8 Gather also verifies explicit nonzero tap offsets, and R8_UNORM GatherAlpha verifies the missing-channel border default. A fully outside 8x anisotropic Texture2D SampleLevel returns its configured border color; edge-crossing 8x anisotropic Texture2D Sample and zero-bias SampleBias footprints centered at `u=0.95` each return `[33, 190, 98, 255]`, as does the corresponding SampleGrad footprint. An 8x anisotropic Texture2DArray implicit Sample, explicit SampleGrad, and zero-bias SampleBias on layer 1 crossing the right edge each return `[0, 151, 104, 255]`. A TextureCube SRV selects the expected color for all six signed-axis directions and blends linear taps across +X/+Y, +X/+Z, and +X/-Y boundaries to `[127, 0, 127, 255]`, `[255, 0, 127, 255]`, and `[255, 127, 0, 255]`. TextureCubeArray SampleLevel passes Naga validation with `CUBE_ARRAY_TEXTURES` enabled; Chromium samples cube indices 0 and 1 from a two-cube, two-mip view starting at face 6, verifies a linear +X/+Y seam, and checks 4x4 mip-0 and 2x2 mip-1 dimensions with zero WebGPU validation errors. A selected RGBA16_FLOAT cube-array view starting at face 6 reads `[64,127,255,255]` and `[255,64,127,255]` from half-float cube texels with one draw and no WebGPU validation errors. D32_FLOAT, D16_UNORM, D24_UNORM_S8, and D32_FLOAT_S8X24 TextureCubeArray SampleCmp and SampleCmpLevelZero using LESS on the same selected two-cube view all return false for cube 0 and true for cube 1 with zero WebGPU validation errors. Other cube-array dimensions/formats remain open. The bundled compiler rejects `ps_6_8` with E5033, so `SampleCmpBias` and `SampleCmpGrad` cannot currently be compiled.
- [x] Verify 8x anisotropic `Texture2DArray.Sample`, `SampleGrad`, and zero-bias `SampleBias` BORDER footprints on R8_UNORM and RG8_UNORM views; Chromium returns `[76, 0, 0, 255]` and `[38, 114, 0, 255]`, confirming normalized edge blends and channel defaults.
- [x] Verify 8x anisotropic `Texture2D.Sample`, `SampleBias`, and `SampleGrad` BORDER footprints on BC4_UNORM; Chromium confirms constant interior samples and channel-correct blends at the compressed texture edge.
- [x] Verify R8_UNORM TextureCubeArray selection on two cubes in a view beginning at face 6; Chromium returns `[128, 0, 0, 255]` and `[64, 0, 0, 255]` and reports 4x4 dimensions, two cubes, and one mip.
- [x] Verify RG8_UNORM TextureCubeArray selection on the same two-cube face range; Chromium returns `[128,64,0,255]` and `[64,192,0,255]`, including missing-channel defaults, and reports two cubes and one mip.
- [x] Verify R16_FLOAT TextureCubeArray samples decode half-floats on the same selected face range; Chromium returns `[128,0,0,255]` and `[64,0,0,255]` for 0.5 and 0.25 and reports two cubes and one mip.
- [x] Verify R16_UNORM and R16_SNORM TextureCubeArray normalization across the same selected face range; Chromium returns `[128,0,0,255]` and `[64,0,0,255]` and reports two cubes and one mip.
- [x] Preserve arbitrary R16_UNORM/SNORM words through upload, `CopyResource`, and staging `Map(READ)`; use R32_FLOAT sampling backing when `float32-filterable` is available, with half-float fallback otherwise. Chromium distinguishes adjacent `0x1234`/`0x1235` source words and verifies sampled values against full 16-bit UNORM/SNORM precision.
- [x] Verify `Texture3D.SampleLevel` BORDER sampling with R8_UNORM; Chromium returns `[0, 0, 0, 255]` outside and `[115, 0, 0, 255]` at a 3D edge footprint.
- [x] Verify `Texture3D.SampleLevel` BORDER sampling with RG8_UNORM; Chromium returns `[0, 0, 0, 255]` outside and `[115, 58, 0, 255]` at a 3D edge footprint, preserving both normalized channels and missing-channel defaults with zero WebGPU errors.
- [x] Verify `Texture3D.SampleLevel` for R16_UNORM and R16_SNORM at outside/edge footprints; Chromium checks `[0, 0, 0, 255]` outside and `[115, 0, 0, 255]` at the edge within 4 channel values, with no WebGPU errors.
- [x] Verify `Texture3D.SampleLevel` for R16_FLOAT at outside/edge footprints; Chromium decodes half-float 0.5, returns `[0, 0, 0, 255]` outside and `[115, 0, 0, 255]` at the edge within 4 channel values, with no WebGPU errors.
- [x] Verify R8_UNORM Texture1DArray samples at mip 1/layer 0 and mip 2/layer 1, plus dimension-query pixels; Chromium returns `[128, 0, 0, 255]` and `[64, 0, 0, 255]` for the samples.
- [x] Verify `CopySubresourceRegion` for a 3D box spanning two depth slices and assert source values plus untouched destination voxels.
- [x] Verify a partial RG8_UNORM 2D `CopySubresourceRegion` copies exact two-channel texels and preserves destination values outside the box.
- [x] Verify partial R16_UNORM/SNORM `CopySubresourceRegion` with half-float fallback preserves copied and untouched normalized words through staging `Map(READ)`.
- [x] Verify a block-aligned BC1 `CopySubresourceRegion` copies one compressed block and leaves its neighbouring block unchanged.
- [x] Verify a block-aligned BC4 `CopySubresourceRegion` copies one compressed block and leaves its neighbouring block unchanged.
- [x] Verify a block-aligned BC3 `CopySubresourceRegion` copies one compressed block and leaves its neighbouring block unchanged.
- [x] Verify block-aligned BC2 and BC5 `CopySubresourceRegion` copies move the selected compressed block and leave each neighbouring block unchanged.
- [x] Verify block-aligned BC6H and BC7 `CopySubresourceRegion` copies move the selected compressed block and leave each neighbouring block unchanged.
- [x] Add D3D11 Texture1D creation with mip and array uploads, default and explicit Texture1DArray SRV ranges, `GetDesc`, and `GetType`; Chromium samples layer 0/mip 1 and layer 1/mip 2 from the 2D-array backing with expected red and green pixels.
- [x] Resolve Texture1DArray and Texture2DArray `GetDimensions`: the retained Naga 30.0.1 source composes `ImageQuery::Size` with `ImageQuery::NumLayers`; shader tests and Chromium pixels verify mip 0/1 widths, two elements, and three mip levels. The rebuilt bridge validates a shader that combines Texture1DArray sampling with the dimension query.
- [x] Verify unaligned 11-byte buffer `CopySubresourceRegion` preserves destination edge bytes and overlapping same-buffer copies use the original source bytes.
- [x] Verify `CopySubresourceRegion` selects the requested 2D-array source and destination mip/layer, copies one texel, and preserves surrounding texels and another mip; fix the browser executor to parse the Rust wire layout.
- [x] Preserve staging `Map(READ)` order across shared CPU slices by executing commands already transferred to the host-visible GPU queue before the current HLE batch; the default shared-worker triangle fixture verifies the staging values.

- [x] Verify RGBA8_SRGB TextureCubeArray selection on two cubes in the same face range; Chromium returns [55, 13, 134, 255] and [13, 134, 55, 128], confirming sRGB RGB decoding with preserved alpha and zero WebGPU errors.
- [x] Verify R10G10B10A2_UNORM TextureCubeArray selection on two cubes in the same face range; Chromium returns `[64,128,192,255]` and `[192,64,128,255]` and reports two cubes and one mip with zero WebGPU validation errors.
- [x] Verify R11G11B10_FLOAT TextureCubeArray selection on two cubes in the same face range; Chromium decodes the packed channels to `[128,64,255,255]` and `[255,128,64,255]` and reports two cubes and one mip with zero WebGPU validation errors.
**Gate:** Every render-state, copy, resolve, packing, and staging item listed open by the checklist has a named passing regression.

### Task 4: Complete compute, UAV, query, and staging paths

**Files:** D3D11 HLE modules, `web/gpu.js`, `test/browser/run.mjs`, `docs/TARGETS.md`

- [x] Enumerate D3D11 typed UAV formats advertised by the HLE and pixel-test all 16 through texture dispatch/readback.
- [x] Lower `RWTexture1DArray` storage images through a 2D-array backing texture; Chromium dispatches across two layers and reads back every written element without WebGPU validation errors.
- [x] Read back D32_FLOAT_S8X24 depth and stencil aspects separately and pack the DXGI 8-byte texel layout; Chromium verifies all four depth/stencil texels.
- [x] Read back D24_UNORM_S8 by sampling its depth-only aspect, quantizing to 24 bits, and copying stencil separately; Chromium checks the packed bytes for all four 2x2 texels. Read back B5G6R5_UNORM, B5G5R5A1_UNORM, and B4G4R4A4_UNORM through RGBA16Float backing; browser assertions verify normalized channel values and exact packed words across odd-width padded 3x2 rows.
- [x] Verify event-query `End`/`GetData` ordering and explicit rejection of unsupported valid query types; invalid query types return `E_INVALIDARG`.
- [x] Verify `SV_DispatchThreadID`, group system values, dispatch-to-draw/readback visibility, and buffer copies feeding later dispatches.
- [x] Report the unsupported D3D11.2 `OPTIONS1` capabilities, including tiled-resource tier 0, with a 16-byte HLE regression.
- [x] Implement deferred contexts and command lists; Rust tests verify deferred-context creation/type/flags, command recording, finish/reset behavior, and execute ordering/state restoration. The browser suite separately verifies that saved WebGPU pipeline state, including nested arrays and maps, restores after clear.
- [x] Verify executor readback of 14 typeless/BGRX DXGI formats using odd-width, padded rows, and all four mip/array subresource indices; D32_FLOAT_S8X24 depth/stencil rows are also verified.

**Gate:** Each supported query/UAV/staging feature has typed value assertions and command-order checks; unsupported feature reporting matches implemented behavior.

### Task 5: Expand real-world shader translation

**Files:** `crates/anywasm-rt/src`, shader fixtures, `web/shaders.js`, `web/gpu.js`, browser tests

- [ ] Convert each reproducible selected-target shader failure into a minimal failing fixture.
- [ ] Implement translation and pipeline-layout fixes, then test module creation, pipeline validation, and pixels/readback.
- [ ] Add shader and pipeline caching plus asynchronous pipeline creation with regressions for cache hits, invalidation, and failure reporting.

**Gate:** Every shader blocker recorded for the selected Diligent target has a passing translation/pipeline regression and the bounded target replay advances past it.

### Task 6: Complete D3D9 fixed-function and device/state coverage

**Files:** `crates/anywasm-rt/src/hle/d3d9.rs`, `crates/anywasm-rt/src/hle/gpu.rs`, `web/gpu.js`, `test/browser/run.mjs`

- [ ] Cover remaining texture operations and multi-stage behavior, including PREMODULATE and bump-environment operations where their required state exists.
- [x] Verify two-stage PREMODULATE for color and alpha: stage 0 passes arg1, then stage 1 SELECTARG1 CURRENT multiplies by its texture; Chromium checks `[0, 128, 64, 64]`. HLE tests reject stage-1 PREMODULATE, bump-environment color/alpha operations, and bump matrix states 7–10 on both stages without mutating state or emitting GPU commands.
- [x] Cover D3D9 viewport/scissor queries and state transitions: `GetViewport` and `GetScissorRect` round-trip their setter state; `CreateDevice` initializes the full-target scissor rectangle, and `SetRenderTarget` resets viewport and scissor state to the new surface bounds. Focused HLE tests check fields and emitted GPU commands. Microsoft documents the full-viewport defaults and target-size resets ([SetRenderTarget](https://learn.microsoft.com/en-us/windows/win32/api/d3d9/nf-d3d9-idirect3ddevice9-setrendertarget), [scissor test](https://learn.microsoft.com/en-us/windows/win32/direct3d9/scissor-test)).
- [ ] Extend state, resource, surface, shader, and device methods from observed target calls; add guest-side and pixel regressions per method.
- [ ] Verify alpha/color argument selectors, stage transitions, and unsupported combinations against the D3D9 operation definitions.

**Gate:** The D3D9 checklist names every implemented state/API slice and pixel-checks all supported fixed-function equations; observed unsupported calls are explicit.

### Task 7: Complete DXGI presentation and D3D12 coverage

**Files:** DXGI/D3D12 HLE modules, `web/gpu.js`, `test/browser/run.mjs`, `docs/BROWSER_SUPPORT.md`

- [ ] Add flip-model swap chains, resize, fullscreen transitions, and present-interval behavior with resource-lifetime and visible-pixel tests.
- [ ] Add a minimal D3D12 device/queue/resource/command-list path and shader/pipeline execution tests for the selected compatibility slice.
- [ ] Verify present scheduling under normal execution and JIT, including device/resize recovery.

**Gate:** Swap-chain state and pixels remain correct across resize/fullscreen/present transitions; each supported D3D12 command path has a readback assertion.

### Task 8: Add OpenGL/WGL and Vulkan paths

**Files:** new or existing graphics HLE modules, `web/gpu.js`, browser fixtures, `CLAUDE.md`

- [ ] Identify the minimum target-grounded WGL/OpenGL and Vulkan entry points and command sequences from a reproducible sample.
- [ ] Implement context/device creation, resource binding, draw/dispatch, and presentation for each selected slice.
- [ ] Add API-level failure tests and browser pixel/readback checks; report adapter/backend limitations precisely.

**Gate:** Each selected API fixture renders known pixels with no validation errors, or the environment-independent API path is fully tested and platform limitations are recorded with evidence.

### Task 9: Finish canvas, frame-pacing, and target acceptance evidence

**Files:** `web/worker.js`, `web/main.js`, `web/gpu.js`, `test/browser/cross-engine.mjs`, `test/browser/target.mjs`, docs

- [x] Verify the worker-transferred OffscreenCanvas blit at 640x480 and after CSS scaling to 320px; Chromium screenshots check the clear and interpolated triangle pixels.
- [x] Verify visible swapchain blit pixels and backing/CSS dimensions at 32x24, then after ResizeBuffers at 96x64, with two presents and zero WebGPU validation errors.
- [x] Verify DPR 2 worker-rendered presentation: the guest backing store remains 640x480, the canvas screenshot scales to 1280 physical pixels wide, clear and triangle pixels match, and three presents report zero validation or page errors.
- [x] Verify present interval semantics and virtual-clock accounting under JIT and worker scheduling. The Chromium worker regression runs the D3D11 textured fixture with emitted blocks enabled, checks three Present(1) waits each wait one vblank, confirms elapsed 100 ns values map to ten-times virtual clock ticks (also covered by the Rust `host_frame_elapsed_time_100ns_is_converted_to_virtual_10ns_ticks` unit test), and verifies three clean presents plus clear/blend/texture backbuffer pixels.
- [x] Recheck seed-1 WASD movement, cursor aim, and flashlight response; fix the SDF row-to-screen Y mapping and record two matching clear-ray runs.
- [x] Recheck random Release Tab map regeneration and record changed-pixel, dispatch, present, and validation-error counts.
- [x] Make stored-route playback dispatch its recorded repeated keydown pulses; verify input events and balanced Asyncify yields/resumes.
- [x] Finish the stored seed-1 route under the current runtime and reach a terminal process status. The corrected centered-square route completed all 35 segments at the production slice, reached the teleport, and reported status 0 after the harness stop.
- [x] Rerun random-layout flashlight ray checks with the corrected SDF row mapping, then update every graphics status/checklist from the resulting evidence. Two fresh random layouts lit all 15 and 17 sampled patches with no WebGPU errors.

**Gate:** The graphics source-of-truth checklists contain no unverified completion claims, every remaining limitation has a concrete reason and reproduction, and all intended commits are pushed with remote SHA verification.
