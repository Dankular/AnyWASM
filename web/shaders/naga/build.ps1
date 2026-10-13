$ErrorActionPreference = 'Stop'

$nagaDir = $PSScriptRoot
$sourceDir = Join-Path $nagaDir 'source'
$manifestPath = Join-Path $sourceDir 'Cargo.toml'
$cargo = (Get-Command cargo -ErrorAction Stop).Source
$wasmBindgen = (Get-Command wasm-bindgen -ErrorAction Stop).Source
$targetDir = Join-Path $env:TEMP 'anywasm-naga-shader-target'
$stageDir = Join-Path $nagaDir ('.stage-' + [Guid]::NewGuid().ToString('N'))
$previousCargoTargetDir = $env:CARGO_TARGET_DIR
$previousRustFlags = $env:RUSTFLAGS

try {
    $env:CARGO_TARGET_DIR = $targetDir
    $requiredRustFlag = '-C link-arg=--max-memory=134217728'
    $env:RUSTFLAGS = if ([string]::IsNullOrWhiteSpace($previousRustFlags)) {
        $requiredRustFlag
    } else {
        "$previousRustFlags $requiredRustFlag"
    }

    $bindgenVersion = (& $wasmBindgen --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $bindgenVersion -notmatch '0\.2\.121$') {
        throw "Expected wasm-bindgen 0.2.121; found '$bindgenVersion'"
    }

    & $cargo build --locked --release --target wasm32-unknown-unknown --manifest-path $manifestPath
    if ($LASTEXITCODE -ne 0) { throw 'Naga bridge Cargo build failed' }

    $wasmPath = Join-Path $targetDir 'wasm32-unknown-unknown\release\winebrowser_shader_wgsl.wasm'
    New-Item -ItemType Directory -Path $stageDir | Out-Null
    & $wasmBindgen --target web --out-name winebrowser_shader_wgsl --out-dir $stageDir $wasmPath
    if ($LASTEXITCODE -ne 0) { throw 'wasm-bindgen generation failed' }

    foreach ($name in @('winebrowser_shader_wgsl.js', 'winebrowser_shader_wgsl_bg.wasm')) {
        Copy-Item -LiteralPath (Join-Path $stageDir $name) -Destination (Join-Path $nagaDir $name) -Force
    }

    $artifacts = [ordered]@{}
    foreach ($name in @('winebrowser_shader_wgsl.js', 'winebrowser_shader_wgsl_bg.wasm')) {
        $artifactPath = Join-Path $nagaDir $name
        $artifacts[$name] = [ordered]@{
            bytes = (Get-Item -LiteralPath $artifactPath).Length
            sha256 = (Get-FileHash -LiteralPath $artifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    $outputManifest = [ordered]@{
        api = 'default init(); spirv_to_wgsl(Uint8Array): string'
        artifacts = $artifacts
        drawParameters = [ordered]@{
            binding = 0
            group = 3
            layout = 'i32[4]: baseVertex, baseInstance, reserved, reserved'
        }
        format = 1
        maximumWasmMemoryBytes = 134217728
        nagaArrayImageQueries = 'ImageQuery::Size plus ImageQuery::NumLayers'
        nagaVersion = '30.0.1'
        package = 'winebrowser-shader-wgsl'
        wasmBindgenVersion = '0.2.121'
    }
    $json = $outputManifest | ConvertTo-Json -Depth 8
    [IO.File]::WriteAllText((Join-Path $nagaDir 'manifest.json'), $json + "`n", [Text.UTF8Encoding]::new($false))
} finally {
    if ($previousCargoTargetDir) { $env:CARGO_TARGET_DIR = $previousCargoTargetDir }
    else { Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue }
    if ($previousRustFlags) { $env:RUSTFLAGS = $previousRustFlags }
    else { Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue }

    if (Test-Path -LiteralPath $stageDir) {
        $resolvedStageDir = [IO.Path]::GetFullPath($stageDir)
        $resolvedNagaDir = [IO.Path]::GetFullPath($nagaDir).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
        if (-not $resolvedStageDir.StartsWith($resolvedNagaDir, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to remove build stage outside the Naga directory: $resolvedStageDir"
        }
        Remove-Item -LiteralPath $resolvedStageDir -Recurse -Force
    }
}
