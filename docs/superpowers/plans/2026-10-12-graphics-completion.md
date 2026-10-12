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

- [ ] Enumerate the untested format/resolve combinations, copy classes, sampler/render-state combinations, and staging layouts from section 3.
- [ ] Add boundary-focused pixel/readback cases for each open behavior, including preservation outside copied/cleared regions.
- [ ] Implement only the failing HLE/executor behavior and rerun the affected browser checks plus the full Chromium suite.
- [x] Forward D3D11 alpha-to-coverage into multisampled WebGPU pipelines; the 4x RGBA8 resolve regression verifies partial red/blue sample coverage from a half-alpha draw in Chromium.
- [x] Forward the `OMSetBlendState` sample mask into WebGPU multisample state and its pipeline cache key; the 4x RGBA8 resolve regression verifies that mask `0x5` preserves two of four samples.
- [x] Pixel-check 4x resolve output for RGBA8, BGRA8, RGBA16_FLOAT, RGBA8_SRGB, BGRA8_SRGB, R10G10B10A2_UNORM, and R11G11B10_FLOAT render targets; the packed normalized and unsigned-float resolve channels match the fractional shader output, and alpha-to-coverage and sample-mask resolves preserve the untouched blue background.
- [x] Report 4x quality levels for D32_FLOAT, D24S8, and D16; accept typeless D24S8 4x depth-resource creation; and pixel-check near/far depth testing with a 4x D24S8 attachment plus RGBA8 color resolve.
- [x] Verify `IndependentBlendEnable` with two render targets: target 0 adds red to its blue clear while target 1 overwrites green with blue.
- [x] Implement D3D11 `SRC1_COLOR`, `INV_SRC1_COLOR`, `SRC1_ALPHA`, and `INV_SRC1_ALPHA` blend factors using the second pixel-shader output; browser pixels verify each factor.
- [x] Verify a D3D11 render-target write mask of `0x5` updates red/blue while preserving green/alpha from the clear.
- [x] Verify independent D3D11 RGB and alpha blend factors/operations with separate pixel expectations.
- [x] Verify D3D11 depth write mask `ZERO` allows a later farther draw through, while `ALL` preserves the nearer result.
- [x] Pixel-check all eight D3D11 depth comparisons with incoming depth below, equal to, and above the cleared reference value.
- [x] Verify stencil write mask `0x0f` preserves upper bits and stencil read mask `0x0f` compares only lower bits.
- [x] Verify D3D11 stencil fail and depth-fail paths for KEEP, ZERO, REPLACE, saturating increment/decrement, invert, wrapping increment/decrement, and masked replace; all boundary pixels pass in the full Chromium suite with no WebGPU validation errors.
- [x] Pixel-check D3D11 WRAP, MIRROR, CLAMP, and MIRROR_ONCE modes and D3D9 fixed-function MIRROR_ONCE; D3D11 BORDER verifies the configured color for point taps and linear footprints outside and straddling 2D RGBA8 edges through Sample, SampleLevel, SampleGrad, and SampleBias, plus 2D-array and 3D RGBA8 edges through SampleLevel. D32_FLOAT comparison BORDER verifies all eight comparison functions for fully outside point-filtered SampleCmpLevelZero taps and for edge-straddling and fully outside linearly filtered SampleCmp footprints. R8_UNORM, RG8_UNORM, B5G6R5_UNORM, BC1, and BC4 BORDER pixels verify view-format channel defaults and format-sensitive variant caching. 2D and 2D-array RGBA8 Gather plus D32_FLOAT GatherCmp verify per-tap border behavior at edge-straddling and fully outside footprints; RGBA8 Gather also verifies explicit nonzero tap offsets, and R8_UNORM GatherAlpha verifies the missing-channel border default. Other sampled dimensions/formats, Shader Model 6.8 bias/gradient comparison methods, cube, and anisotropic BORDER cases remain open.
- [x] Verify `CopySubresourceRegion` for a 3D box spanning two depth slices and assert source values plus untouched destination voxels.
- [x] Verify unaligned 11-byte buffer `CopySubresourceRegion` preserves destination edge bytes and overlapping same-buffer copies use the original source bytes.

**Gate:** Every render-state, copy, resolve, packing, and staging item listed open by the checklist has a named passing regression.

### Task 4: Complete compute, UAV, query, and staging paths

**Files:** D3D11 HLE modules, `web/gpu.js`, `test/browser/run.mjs`, `docs/TARGETS.md`

- [x] Enumerate D3D11 typed UAV formats advertised by the HLE and pixel-test all 16 through texture dispatch/readback.
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
- [ ] Repeat selected-target rendering/input/gameplay checks and update every graphics status/checklist from the resulting evidence.

**Gate:** The graphics source-of-truth checklists contain no unverified completion claims, every remaining limitation has a concrete reason and reproduction, and all intended commits are pushed with remote SHA verification.
