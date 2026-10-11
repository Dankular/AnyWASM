# Browser support evidence

This matrix records exercised behavior, not a compatibility promise. The Chromium browser suite is
`test/browser/run.mjs`; it launches headless Chromium through Playwright with
`--enable-unsafe-webgpu` and `--enable-features=Vulkan`. `CHROMIUM` can select the executable;
otherwise the script expects a Chromium under `/opt/pw-browsers`. The cross-engine smoke check is
`test/browser/cross-engine.mjs`; `CHROMIUM`, `FIREFOX`, and `WEBKIT` can select executables, and
`BROWSER` can select one engine.

Fresh local run on 2026-10-11: Chrome `154.0.8037.98`, selected with `CHROMIUM` on Windows x64;
`node test/browser/run.mjs` passed all checks. WebGPU reported an NVIDIA adapter, zero validation
errors, and successful triangle/textured back-buffer checks. This was still a headless `?test=1`
run, so it does not establish visible-canvas pixel output.

Fresh local cross-engine run on 2026-10-11: Firefox `141.0` and Playwright WebKit `18.2`, on Windows
x64. The hello fixture exited with code 0 in both. Firefox completed the D3D11 triangle fixture in
three frames. WebKit reported no WebGPU and no `transferControlToOffscreen`; the triangle stopped
with a clear `Graphics unavailable: OffscreenCanvas worker transfer is unsupported` status. A
synthetic cache-pressure probe compiled 300 unique emitted modules in Node, Firefox, and WebKit,
then verified that evicted modules recompile and retained modules execute from cache. The WebKit
result verifies explicit failure handling, not rendering support or native Safari behavior.

Fresh CSP run on 2026-10-11: Chromium `151.0.7922.34`, Firefox `141.0`, and Playwright WebKit `18.2`
loaded the page and same-origin worker with `default-src 'self'; script-src 'self'
'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; worker-src 'self'; connect-src 'self'`.
The hello worker exited successfully, and the emitted-module cache-pressure probe compiled and ran
without fallback in all three engines. This verifies that policy's requirements; it is not a general
CSP deployment guide.

| Browser or feature | Evidence and current status |
|---|---|
| Chromium, headless | **Partial, exercised by the browser suite.** It drives the real page and worker; checks load progress and accessible unimplemented-import diagnostics; sends keyboard and mouse input to a guest window; and compares opt-in emitted execution with cached execution. |
| Content Security Policy | **Partial, exercised in Chromium, Firefox, and WebKit.** A same-origin policy allowing `wasm-unsafe-eval` permits the page and worker to run hello and the emitted-module cache-pressure probe. Other deployment policies and directives remain untested. |
| WebGPU through Chromium | **Partial, exercised by the browser suite.** D3D11 triangle and textured fixtures are checked by reading back the GPU back buffer in `?test=1` mode. A separate non-test-mode run captures the visible 640x480 triangle canvas and checks its clear colour and interpolated centre pixel. Other rendering paths and browser engines remain unverified. |
| Firefox | **Partial, smoke tested.** Firefox 141.0 exits the hello fixture and completes the D3D11 triangle program in three frames. Pixel output, input behavior, and the broader conformance matrix remain untested. |
| Safari / WebKit | **Partial, smoke tested with Playwright WebKit 18.2.** Hello exits successfully. The installed engine lacks WebGPU and OffscreenCanvas worker transfer; graphics programs stop with an explicit limitation. This does not establish native Safari behavior. |
| Large-game memory, `memory64`, asset streaming | **Unmeasured / open.** The build target is `wasm32-unknown-unknown`. The browser buffers the complete `.awb` as an `ArrayBuffer`, copies it into WASM linear memory, then `Bundle::from_bytes` copies each file into the in-memory VFS. No 4 GB pressure test, memory64 build, or large-asset streaming test has run; this is a source-level assessment, not a browser measurement. |

The checked-in assertions establish their scope; they do not test the GLFW Demo or a real game.
Firefox and WebKit have only smoke coverage outside Chromium. No full cross-browser support claim
should be made based on these runs. See [`CLAUDE.md`](../CLAUDE.md) for the project checklist and
[`STATUS.md`](STATUS.md) for implementation limits.
