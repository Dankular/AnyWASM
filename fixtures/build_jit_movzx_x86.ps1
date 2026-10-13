$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$source = Join-Path $PSScriptRoot 'jit_movzx_x86.c'
$output = Join-Path $PSScriptRoot 'jit_movzx_x86.exe'
$targetDir = Join-Path $repoRoot 'target\jit-movzx-x86'
$object = Join-Path $targetDir 'jit_movzx_x86.obj'
$batchPath = Join-Path $targetDir 'build.cmd'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'

if (-not (Test-Path -LiteralPath $vswhere)) {
    throw "Visual Studio locator was not found: $vswhere"
}

$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath |
    Select-Object -First 1
if (-not $installation) {
    throw 'No Visual Studio installation with the x86 C++ toolchain was found.'
}

$devCmd = Join-Path $installation 'Common7\Tools\VsDevCmd.bat'
if (-not (Test-Path -LiteralPath $devCmd)) {
    throw "Visual Studio developer command script was not found: $devCmd"
}

New-Item -ItemType Directory -Force -Path $targetDir | Out-Null
$commands = @(
    '@echo off',
    'set PATH=%SystemRoot%\System32;%SystemRoot%;%SystemRoot%\System32\Wbem;%SystemRoot%\System32\WindowsPowerShell\v1.0',
    ('call "{0}" -no_logo -arch=x86 -host_arch=x64' -f $devCmd),
    'if errorlevel 1 exit /b %errorlevel%',
    ('cl.exe /nologo /c /GS- /Od /Zl /Fo:"{0}" "{1}"' -f $object, $source),
    'if errorlevel 1 exit /b %errorlevel%',
    ('link.exe /nologo /machine:x86 /subsystem:console /entry:main /nodefaultlib /fixed /base:0x400000 /out:"{0}" "{1}"' -f $output, $object),
    'if errorlevel 1 exit /b %errorlevel%'
)
[System.IO.File]::WriteAllLines($batchPath, $commands, [System.Text.Encoding]::ASCII)

$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $env:ComSpec
$startInfo.Arguments = '/d /c "' + $batchPath + '"'
$startInfo.UseShellExecute = $false
$process = [System.Diagnostics.Process]::Start($startInfo)
$process.WaitForExit()
Remove-Item -LiteralPath $batchPath -Force
if ($process.ExitCode -ne 0) {
    throw "PE32 MOVZX fixture build failed (exit $($process.ExitCode))."
}

Write-Host "Built $output"
