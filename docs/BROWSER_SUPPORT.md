# Browser support evidence

This matrix records exercised behavior, not a compatibility promise. The current browser suite is
`test/browser/run.mjs`; it launches headless Chromium through Playwright with
`--enable-unsafe-webgpu` and `--enable-features=Vulkan`. `CHROMIUM` can select the executable;
otherwise the script expects a Chromium under `/opt/pw-browsers`.

Fresh local run on 2026-10-11: Chrome `154.0.8037.98`, selected with `CHROMIUM` on Windows x64;
`node test/browser/run.mjs` passed all checks. WebGPU reported an NVIDIA adapter, zero validation
errors, and successful triangle/textured back-buffer checks. This was still a headless `?test=1`
run, so it does not establish visible-canvas pixel output.

| Browser or feature | Evidence and current status |
|---|---|
| Chromium, headless | **Partial, exercised by the browser suite.** It drives the real page and worker; checks load progress and accessible unimplemented-import diagnostics; sends keyboard and mouse input to a guest window; and compares opt-in emitted execution with cached execution. |
| WebGPU through Chromium | **Partial, exercised by the browser suite.** D3D11 triangle and textured fixtures are checked by reading back the GPU back buffer in `?test=1` mode. A separate non-test-mode run captures the visible 640x480 triangle canvas and checks its clear colour and interpolated centre pixel. Other rendering paths and browser engines remain unverified. |
| Firefox | **Untested.** No Firefox launch or conformance run is in the harness. |
| Safari / WebKit | **Untested.** No Safari or WebKit launch or conformance run is in the harness. |
| Large-game memory, `memory64`, asset streaming | **Unmeasured / open.** The current fixtures do not exercise 4 GB memory pressure, memory64, or streaming of large asset sets. |

The Chromium suite's checked-in assertions establish its scope; they do not test the GLFW Demo or a
real game. No cross-browser support claim should be made until corresponding runs are added and
verified. See [`CLAUDE.md`](../CLAUDE.md) for the project checklist and
[`STATUS.md`](STATUS.md) for implementation limits.
