param(
  [string]$InstallDir = 'C:\Program Files (x86)\Rockstar Games\GTA San Andreas',
  [string]$Output = 'web/demo/gta_sa.awb',
  [string]$AssetOutput = 'web/demo/gta_sa.agp'
)

$ErrorActionPreference = 'Stop'
$InstallDir = (Resolve-Path -LiteralPath $InstallDir).Path
$exe = Join-Path $InstallDir 'gta_sa_compact1.0.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
  throw "The selected game executable was not found: $exe"
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$outputPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Output))
$assetOutputPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $AssetOutput))
if ($outputPath.Equals($assetOutputPath, [StringComparison]::OrdinalIgnoreCase)) {
  throw 'The runtime bundle and streamed asset package must use different output paths.'
}
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $outputPath) | Out-Null
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $assetOutputPath) | Out-Null

# Keep the selected executable and every guest DLL close to the runtime. Other files stay in the
# range-readable data package, including audio streams and language-specific banks.
$excluded = @('gta_sa.exe', 'gta_sa1.0.exe') | ForEach-Object { $_.Replace('/', '\').Trim('\') }
$files = @(Get-ChildItem -LiteralPath $InstallDir -File -Recurse -Force | ForEach-Object {
  $relative = $_.FullName.Substring($InstallDir.TrimEnd('\').Length).TrimStart('\').Replace('/', '\')
  $skip = $relative -match '(^|\\)ReadMe(?:\.[^\\]*)?$'
  foreach ($prefix in $excluded) {
    if ($relative.Equals($prefix, [StringComparison]::OrdinalIgnoreCase) -or
        $relative.StartsWith($prefix + '\', [StringComparison]::OrdinalIgnoreCase)) {
      $skip = $true
      break
    }
  }
  if (-not $skip) {
    [pscustomobject]@{ Name = $relative; Path = $_.FullName; Length = [UInt64]$_.Length }
  }
} | Sort-Object { $_.Name.ToLowerInvariant() })

$bootFiles = @($files | Where-Object {
  $_.Name.Equals('gta_sa_compact1.0.exe', [StringComparison]::OrdinalIgnoreCase) -or
  [System.IO.Path]::GetExtension($_.Name).Equals('.dll', [StringComparison]::OrdinalIgnoreCase)
} | Sort-Object { $_.Name.ToLowerInvariant() })
$assetFiles = @($files | Where-Object {
  -not $_.Name.Equals('gta_sa_compact1.0.exe', [StringComparison]::OrdinalIgnoreCase) -and
  -not [System.IO.Path]::GetExtension($_.Name).Equals('.dll', [StringComparison]::OrdinalIgnoreCase)
} | Sort-Object { $_.Name.ToLowerInvariant() })
if (-not ($bootFiles | Where-Object { $_.Name.Equals('gta_sa_compact1.0.exe', [StringComparison]::OrdinalIgnoreCase) })) {
  throw 'The selected compact executable is missing from the runtime bundle file list.'
}

$buffer = [byte[]]::new(4MB)
$copyFile = {
  param([System.IO.BinaryWriter]$Writer, [string]$Path)
  $source = [System.IO.File]::OpenRead($Path)
  try {
    while (($read = $source.Read($buffer, 0, $buffer.Length)) -gt 0) {
      $Writer.Write($buffer, 0, $read)
    }
  } finally {
    $source.Dispose()
  }
}

# AWB1 contains only the executable, DLLs, and its small manifest.
$manifest = [System.Text.Encoding]::UTF8.GetBytes("exe=gta_sa_compact1.0.exe`n")
$bundleEntries = [System.Collections.Generic.List[object]]::new()
$bundleEntries.Add([pscustomobject]@{ Name = '__manifest__'; Path = $null; Length = [UInt64]$manifest.Length })
foreach ($file in $bootFiles) { $bundleEntries.Add($file) }
$stream = [System.IO.File]::Open($outputPath, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
$writer = [System.IO.BinaryWriter]::new($stream, [System.Text.Encoding]::UTF8, $true)
try {
  $writer.Write([byte[]]@(65, 87, 66, 49)) # AWB1
  $writer.Write([UInt32]$bundleEntries.Count)
  foreach ($entry in $bundleEntries) {
    $nameBytes = [System.Text.Encoding]::UTF8.GetBytes($entry.Name)
    if ($nameBytes.Length -gt [UInt16]::MaxValue) { throw "Bundle path is too long: $($entry.Name)" }
    $writer.Write([UInt16]$nameBytes.Length)
    $writer.Write($nameBytes)
    $writer.Write([UInt64]$entry.Length)
  }
  $writer.Write($manifest)
  foreach ($file in $bootFiles) { & $copyFile $writer $file.Path }
} finally {
  $writer.Dispose()
  $stream.Dispose()
}

# AGP1 index records point directly to uncompressed file bytes so HTTP ranges map one-to-one.
[UInt64]$indexLength = 0
foreach ($file in $assetFiles) {
  $nameBytes = [System.Text.Encoding]::UTF8.GetBytes($file.Name)
  if ($nameBytes.Length -gt [UInt16]::MaxValue) { throw "Asset path is too long: $($file.Name)" }
  $indexLength += [UInt64](2 + $nameBytes.Length + 16)
}
[UInt64]$dataOffset = 24 + $indexLength
[UInt64]$packageLength = $dataOffset
foreach ($file in $assetFiles) { $packageLength += [UInt64]$file.Length }
$stream = [System.IO.File]::Open($assetOutputPath, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
$writer = [System.IO.BinaryWriter]::new($stream, [System.Text.Encoding]::UTF8, $true)
try {
  $writer.Write([byte[]]@(65, 71, 80, 49)) # AGP1
  $writer.Write([UInt32]$assetFiles.Count)
  $writer.Write([UInt64]$indexLength)
  $writer.Write([UInt64]$packageLength)
  foreach ($file in $assetFiles) {
    $nameBytes = [System.Text.Encoding]::UTF8.GetBytes($file.Name)
    $writer.Write([UInt16]$nameBytes.Length)
    $writer.Write($nameBytes)
    $writer.Write([UInt64]$dataOffset)
    $writer.Write([UInt64]$file.Length)
    $dataOffset += [UInt64]$file.Length
  }
  foreach ($file in $assetFiles) { & $copyFile $writer $file.Path }
} finally {
  $writer.Dispose()
  $stream.Dispose()
}

$bundleSize = (Get-Item -LiteralPath $outputPath).Length
$assetSize = (Get-Item -LiteralPath $assetOutputPath).Length
Write-Host ("GTA launch bundle ready: {0} ({1:N1} MiB, {2} startup files)" -f $outputPath, ($bundleSize / 1MB), $bootFiles.Count)
Write-Host ("GTA streamed data ready: {0} ({1:N2} GiB, {2} indexed files)" -f $assetOutputPath, ($assetSize / 1GB), $assetFiles.Count)
