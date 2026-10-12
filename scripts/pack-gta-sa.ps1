param(
  [string]$InstallDir = 'C:\Program Files (x86)\Rockstar Games\GTA San Andreas',
  [string]$Output = 'web/demo/gta_sa.awb'
)

$ErrorActionPreference = 'Stop'
$InstallDir = (Resolve-Path -LiteralPath $InstallDir).Path
$exe = Join-Path $InstallDir 'gta_sa_compact1.0.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
  throw "The selected game executable was not found: $exe"
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$outputPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Output))
$outputDir = Split-Path -Parent $outputPath
New-Item -ItemType Directory -Force -Path $outputDir | Out-Null

# Keep the compact entry binary, core game data, and general SFX. The radio/music,
# language-specific voice banks, and alternate executables are omitted to fit WASM32 memory.
$excluded = @(
  'audio\streams',
  'audio\SFX\SPC_GA',
  'audio\SFX\SPC_NA',
  'audio\SFX\SPC_PA',
  'audio\SFX\SPC_EA',
  'audio\SFX\SPC_FA',
  'audio\SFX\SCRIPT',
  'gta_sa.exe',
  'gta_sa1.0.exe',
  'ReadMe'
) | ForEach-Object { $_.Replace('/', '\').Trim('\') }

$files = Get-ChildItem -LiteralPath $InstallDir -File -Recurse | ForEach-Object {
  $relative = $_.FullName.Substring($InstallDir.TrimEnd('\').Length).TrimStart('\').Replace('/', '\')
  $skip = $false
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
} | Sort-Object Name

$manifest = [System.Text.Encoding]::UTF8.GetBytes("exe=gta_sa_compact1.0.exe`n")
$entries = [System.Collections.Generic.List[object]]::new()
$entries.Add([pscustomobject]@{ Name = '__manifest__'; Path = $null; Length = [UInt64]$manifest.Length })
foreach ($file in $files) {
  $entries.Add($file)
}

$stream = [System.IO.File]::Open($outputPath, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
$writer = [System.IO.BinaryWriter]::new($stream, [System.Text.Encoding]::UTF8, $true)
try {
  $writer.Write([byte[]]@(65, 87, 66, 49)) # AWB1
  $writer.Write([UInt32]$entries.Count)
  foreach ($entry in $entries) {
    $nameBytes = [System.Text.Encoding]::UTF8.GetBytes($entry.Name)
    if ($nameBytes.Length -gt [UInt16]::MaxValue) { throw "Bundle path is too long: $($entry.Name)" }
    $writer.Write([UInt16]$nameBytes.Length)
    $writer.Write($nameBytes)
    $writer.Write([UInt64]$entry.Length)
  }

  $writer.Write($manifest)
  $buffer = [byte[]]::new(4MB)
  foreach ($entry in $files) {
    $source = [System.IO.File]::OpenRead($entry.Path)
    try {
      while (($read = $source.Read($buffer, 0, $buffer.Length)) -gt 0) {
        $writer.Write($buffer, 0, $read)
      }
    } finally {
      $source.Dispose()
    }
  }
} finally {
  $writer.Dispose()
  $stream.Dispose()
}

$bundleSize = (Get-Item -LiteralPath $outputPath).Length
Write-Host ("GTA bundle ready: {0} ({1:N2} GiB, {2} files)" -f $outputPath, ($bundleSize / 1GB), $files.Count)
