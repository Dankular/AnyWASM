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
- [x] Verify `IndependentBlendEnable` with two render targets: target 0 adds red to its blue clear while target 1 overwrites green with blue.
- [x] Implement D3D11 `SRC1_COLOR`, `INV_SRC1_COLOR`, `SRC1_ALPHA`, and `INV_SRC1_ALPHA` blend factors using the second pixel-shader output; browser pixels verify each factor.
- [x] Verify a D3D11 render-target write mask of `0x5` updates red/blue while preserving green/alpha from the clear.
- [x] Verify D3D11 depth write mask `ZERO` allows a later farther draw through, while `ALL` preserves the nearer result.

**Gate:** Every render-state, copy, resolve, packing, and staging item listed open by the checklist has a named passing regression.

### Task 4: Complete compute, UAV, query, and staging paths

**Files:** D3D11 HLE modules, `web/gpu.js`, `test/browser/run.mjs`, `docs/TARGETS.md`

- [ ] Add tests for remaining typed UAV formats, general staging readback, and query types required by the selected API contract.
- [ ] Add focused coverage for additional system values and synchronization/order between dispatch, draw, copy, and readback.
- [ ] Implement deferred contexts/command lists and tiled resources, or provide exact feature-capability behavior and tests required by D3D11 compatibility.

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

- [ ] Cover canvas resize, offscreen transfer, HiDPI, and visible blit pixels at multiple canvas sizes.
- [ ] Verify present interval semantics and virtual-clock accounting under JIT and worker scheduling.
- [ ] Repeat selected-target rendering/input/gameplay checks and update every graphics status/checklist from the resulting evidence.

**Gate:** The graphics source-of-truth checklists contain no unverified completion claims, every remaining limitation has a concrete reason and reproduction, and all intended commits are pushed with remote SHA verification.
