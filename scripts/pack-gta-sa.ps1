param(
  [string]$InstallDir = 'C:\Program Files (x86)\Rockstar Games\GTA San Andreas',
  [string]$Output = 'web/demo/gta_sa.awb',
  [string]$AssetOutput = 'web/demo/gta_sa.agp',
  [string]$PackageOutput = 'web/demo/gta_sa.package.json',
  [string]$CatalogOutput = 'web/demo/gta_sa.catalog.json',
  [string]$Version = '1.0.0',
  [switch]$MetadataOnly
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
$packageOutputPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $PackageOutput))
$catalogOutputPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $CatalogOutput))
if ($Version -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$') {
  throw "Package version must be a semantic version: $Version"
}
$outputPaths = @($exe, $outputPath, $assetOutputPath, $packageOutputPath, $catalogOutputPath)
for ($i = 0; $i -lt $outputPaths.Count; $i++) {
  for ($j = $i + 1; $j -lt $outputPaths.Count; $j++) {
    if ($outputPaths[$i].Equals($outputPaths[$j], [StringComparison]::OrdinalIgnoreCase)) {
      throw "Input and generated package files must use distinct paths: $($outputPaths[$i])"
    }
  }
}
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $outputPath) | Out-Null
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $assetOutputPath) | Out-Null
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $packageOutputPath) | Out-Null
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $catalogOutputPath) | Out-Null

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

# Determine the exact index size before either writing or validating the package files.
[UInt64]$indexLength = 0
foreach ($file in $assetFiles) {
  $nameBytes = [System.Text.Encoding]::UTF8.GetBytes($file.Name)
  if ($nameBytes.Length -gt [UInt16]::MaxValue) { throw "Asset path is too long: $($file.Name)" }
  $indexLength += [UInt64](2 + $nameBytes.Length + 16)
}
if ($indexLength -gt (64MB - 24)) { throw 'The AGP index exceeds the browser package manager limit of 64 MiB.' }
[UInt64]$dataOffset = 24 + $indexLength
[UInt64]$packageLength = $dataOffset
foreach ($file in $assetFiles) { $packageLength += [UInt64]$file.Length }

# AWB1 contains only the executable, DLLs, and its small manifest.
$manifest = [System.Text.Encoding]::UTF8.GetBytes("exe=gta_sa_compact1.0.exe`n")
$bundleEntries = [System.Collections.Generic.List[object]]::new()
$bundleEntries.Add([pscustomobject]@{ Name = '__manifest__'; Path = $null; Length = [UInt64]$manifest.Length })
foreach ($file in $bootFiles) { $bundleEntries.Add($file) }
if ($MetadataOnly) {
  if (-not (Test-Path -LiteralPath $outputPath -PathType Leaf) -or -not (Test-Path -LiteralPath $assetOutputPath -PathType Leaf)) {
    throw 'MetadataOnly requires existing AWB and AGP package files.'
  }
  $bundleReader = [System.IO.BinaryReader]::new([System.IO.File]::OpenRead($outputPath))
  try {
    if ([System.Text.Encoding]::ASCII.GetString($bundleReader.ReadBytes(4)) -ne 'AWB1') { throw 'Existing runtime bundle is not AWB1.' }
    $entryCount = $bundleReader.ReadUInt32()
    [UInt64]$payloadLength = 0
    $hasSelectedExe = $false
    for ($i = 0; $i -lt $entryCount; $i++) {
      $nameLength = $bundleReader.ReadUInt16()
      $name = [System.Text.Encoding]::UTF8.GetString($bundleReader.ReadBytes($nameLength))
      $length = $bundleReader.ReadUInt64()
      $payloadLength += $length
      if ($name.Equals('gta_sa_compact1.0.exe', [StringComparison]::OrdinalIgnoreCase)) { $hasSelectedExe = $true }
    }
    if (-not $hasSelectedExe) { throw 'Existing runtime bundle does not contain gta_sa_compact1.0.exe.' }
    if (($bundleReader.BaseStream.Position + $payloadLength) -ne $bundleReader.BaseStream.Length) { throw 'Existing AWB entry lengths do not match its file size.' }
  } finally { $bundleReader.Dispose() }

  $assetReader = [System.IO.BinaryReader]::new([System.IO.File]::OpenRead($assetOutputPath))
  try {
    if ([System.Text.Encoding]::ASCII.GetString($assetReader.ReadBytes(4)) -ne 'AGP1') { throw 'Existing streamed package is not AGP1.' }
    $count = $assetReader.ReadUInt32()
    $foundIndexLength = $assetReader.ReadUInt64()
    $foundPackageLength = $assetReader.ReadUInt64()
    if ($count -ne $assetFiles.Count -or $foundIndexLength -ne $indexLength -or $foundPackageLength -ne $packageLength -or $foundPackageLength -ne $assetReader.BaseStream.Length) {
      throw 'Existing AGP package does not match the current installed game files; run the full packer without MetadataOnly.'
    }
    [UInt64]$expectedOffset = 24 + $foundIndexLength
    foreach ($file in $assetFiles) {
      $nameLength = $assetReader.ReadUInt16()
      $name = [System.Text.Encoding]::UTF8.GetString($assetReader.ReadBytes($nameLength))
      $offset = $assetReader.ReadUInt64()
      $length = $assetReader.ReadUInt64()
      if (-not $name.Equals($file.Name, [StringComparison]::OrdinalIgnoreCase) -or $offset -ne $expectedOffset -or $length -ne $file.Length) {
        throw "Existing AGP index does not match the installed game file: $($file.Name)"
      }
      $expectedOffset += $length
    }
    if ($assetReader.BaseStream.Position -ne (24 + $foundIndexLength)) { throw 'Existing AGP index length does not match its records.' }
  } finally { $assetReader.Dispose() }
} else {
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
}

$bundleSize = (Get-Item -LiteralPath $outputPath).Length
$assetSize = (Get-Item -LiteralPath $assetOutputPath).Length
$bundleSha256 = (Get-FileHash -LiteralPath $outputPath -Algorithm SHA256).Hash.ToLowerInvariant()
$indexBytes = [byte[]]::new([int](24 + $indexLength))
$indexStream = [System.IO.File]::OpenRead($assetOutputPath)
try {
  $copied = 0
  while ($copied -lt $indexBytes.Length) {
    $read = $indexStream.Read($indexBytes, $copied, $indexBytes.Length - $copied)
    if ($read -le 0) { throw 'The AGP header/index was truncated after packaging.' }
    $copied += $read
  }
} finally {
  $indexStream.Dispose()
}
$sha = [System.Security.Cryptography.SHA256]::Create()
try { $indexSha256 = [BitConverter]::ToString($sha.ComputeHash($indexBytes)).Replace('-', '').ToLowerInvariant() }
finally { $sha.Dispose() }
$relativeUrl = {
  param([string]$BaseFile, [string]$TargetFile)
  $baseDirectory = [System.IO.Path]::GetDirectoryName($BaseFile).TrimEnd('\') + '\'
  $baseUri = [System.Uri]::new($baseDirectory)
  $targetUri = [System.Uri]::new($TargetFile)
  $baseUri.MakeRelativeUri($targetUri).ToString()
}
$package = [ordered]@{
  schemaVersion = 1
  id = 'gta-san-andreas'
  version = $Version
  name = 'Grand Theft Auto: San Andreas'
  entry = 'gta_sa_compact1.0.exe'
  dependencies = @()
  bundle = [ordered]@{
    url = (& $relativeUrl $packageOutputPath $outputPath)
    size = [Int64]$bundleSize
    sha256 = $bundleSha256
  }
  assets = [ordered]@{
    url = (& $relativeUrl $packageOutputPath $assetOutputPath)
    size = [Int64]$assetSize
    indexSha256 = $indexSha256
  }
}
$catalog = [ordered]@{
  schemaVersion = 1
  name = 'GTA San Andreas'
  packages = @([ordered]@{
    id = 'gta-san-andreas'
    version = $Version
    name = 'Grand Theft Auto: San Andreas'
    summary = 'User-supplied GTA San Andreas files streamed by AnyWASM.'
    manifest = (& $relativeUrl $catalogOutputPath $packageOutputPath)
  })
}
$jsonEncoding = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText($packageOutputPath, ($package | ConvertTo-Json -Depth 8), $jsonEncoding)
[System.IO.File]::WriteAllText($catalogOutputPath, ($catalog | ConvertTo-Json -Depth 8), $jsonEncoding)
Write-Host ("GTA launch bundle ready: {0} ({1:N1} MiB, {2} startup files)" -f $outputPath, ($bundleSize / 1MB), $bootFiles.Count)
Write-Host ("GTA streamed data ready: {0} ({1:N2} GiB, {2} indexed files)" -f $assetOutputPath, ($assetSize / 1GB), $assetFiles.Count)
Write-Host ("Package manifest ready: {0} (bundle SHA-256 and AGP index SHA-256 recorded)" -f $packageOutputPath)
Write-Host ("Catalog ready: {0} (add this URL in the browser package library)" -f $catalogOutputPath)
