[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$source = Join-Path $PSScriptRoot 'exception_fixture.cpp'
$outputDirectory = Join-Path $repoRoot 'target\msvc-cpp-eh'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'

if (-not (Test-Path -LiteralPath $vswhere)) {
    throw "Visual Studio locator was not found: $vswhere"
}

$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath |
    Select-Object -First 1
if (-not $installation) {
    throw 'No Visual Studio installation with the x64 C++ toolchain was found.'
}

$devCmd = Join-Path $installation 'Common7\Tools\VsDevCmd.bat'
if (-not (Test-Path -LiteralPath $devCmd)) {
    throw "Visual Studio developer command script was not found: $devCmd"
}

New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null

$setup = 'call "{0}" -no_logo -arch=x64 -host_arch=x64' -f $devCmd

$variants = @(
    @{ Name = 'eh4-dynamic'; ExtraFlags = '' },
    @{ Name = 'eh3-dynamic'; ExtraFlags = '/d2FH4-' }
)

$commands = @(
    '@echo off',
    'set PATH=%SystemRoot%\System32;%SystemRoot%;%SystemRoot%\System32\Wbem;%SystemRoot%\System32\WindowsPowerShell\v1.0',
    $setup,
    'if errorlevel 1 exit /b %errorlevel%'
)
foreach ($variant in $variants) {
    $output = Join-Path $outputDirectory ($variant.Name + '.exe')
    $compilerVersion = if ($variant.Name -eq 'eh4-dynamic') { '/Bv ' } else { '' }
    $commands += 'cl.exe /nologo ' + $compilerVersion + '/MD /EHsc /std:c++17 /Od /Ob0 /W4 ' +
        $variant.ExtraFlags + ' /Fe:"' + $output + '" "' + $source + '"'
    $commands += 'if errorlevel 1 exit /b %errorlevel%'
}
$x64SehOutput = Join-Path $outputDirectory 'seh-x64.exe'
$sehSource = Join-Path $PSScriptRoot 'seh_fixture.c'
$x64SehObject = Join-Path $outputDirectory 'seh_fixture_x64.obj'
$commands += 'cl.exe /nologo /MD /Od /W4 /Fo:"' + $x64SehObject + '" /Fe:"' + $x64SehOutput + '" "' + $sehSource + '"'
$commands += 'if errorlevel 1 exit /b %errorlevel%'

# PE32 uses the legacy absolute-pointer EH3 format.
$setupX86 = 'call "{0}" -no_logo -arch=x86 -host_arch=x64' -f $devCmd
$commands += $setupX86
$commands += 'if errorlevel 1 exit /b %errorlevel%'
$x86StartupOutput = Join-Path $outputDirectory 'eh3-x86-dynamic.exe'
$commands += 'cl.exe /nologo /MD /EHsc /std:c++17 /Od /Ob0 /W4 /d2FH4- ' +
    '/Fe:"' + $x86StartupOutput + '" "' + $source + '"'
$commands += 'if errorlevel 1 exit /b %errorlevel%'
# Direct entry isolates C++ EH from the separate EH4 wrapper used by the MSVC CRT startup object.
$x86Output = Join-Path $outputDirectory 'eh3-x86-direct.exe'
$commands += 'cl.exe /nologo /MD /EHsc /std:c++17 /Od /Ob0 /GS- /W4 /d2FH4- ' +
    '/Fe:"' + $x86Output + '" "' + $source + '" ' +
    '/link /ENTRY:main /DEFAULTLIB:vcruntime.lib /DEFAULTLIB:ucrt.lib'
$commands += 'if errorlevel 1 exit /b %errorlevel%'
$x86TerminateOutput = Join-Path $outputDirectory 'terminate-x86.exe'
$x86TerminateSource = Join-Path $PSScriptRoot 'terminate_x86_fixture.cpp'
$commands += 'cl.exe /nologo /MD /EHsc /std:c++17 /Od /Ob0 /GS- /W4 /d2FH4- ' +
    '/Fe:"' + $x86TerminateOutput + '" "' + $x86TerminateSource + '" ' +
    '/link /ENTRY:main /DEFAULTLIB:vcruntime.lib /DEFAULTLIB:ucrt.lib'
$commands += 'if errorlevel 1 exit /b %errorlevel%'
$x86TerminateUnhandledOutput = Join-Path $outputDirectory 'terminate-unhandled-x86.exe'
$commands += 'cl.exe /nologo /MD /EHsc /std:c++17 /Od /Ob0 /GS- /W4 /d2FH4- /DTERMINATE_UNHANDLED ' +
    '/Fe:"' + $x86TerminateUnhandledOutput + '" "' + $x86TerminateSource + '" ' +
    '/link /ENTRY:main /DEFAULTLIB:vcruntime.lib /DEFAULTLIB:ucrt.lib'
$commands += 'if errorlevel 1 exit /b %errorlevel%'
$x86SehOutput = Join-Path $outputDirectory 'seh-x86.exe'
$x86SehSource = $sehSource
$x86SehObject = Join-Path $outputDirectory 'seh_fixture.obj'
$commands += 'cl.exe /nologo /MD /Od /W4 /Fo:"' + $x86SehObject + '" /Fe:"' + $x86SehOutput + '" "' + $x86SehSource + '"'
$commands += 'if errorlevel 1 exit /b %errorlevel%'

$batchPath = Join-Path $outputDirectory 'build-fixtures.cmd'
[System.IO.File]::WriteAllLines($batchPath, $commands, [System.Text.Encoding]::ASCII)
$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $env:ComSpec
$startInfo.Arguments = '/d /c "' + $batchPath + '"'
$startInfo.UseShellExecute = $false
$process = [System.Diagnostics.Process]::Start($startInfo)
$process.WaitForExit()
Remove-Item -LiteralPath $batchPath -Force
if ($process.ExitCode -ne 0) {
    throw "MSVC fixture build failed (exit $($process.ExitCode))."
}

Write-Host "Built EH fixtures in $outputDirectory"
