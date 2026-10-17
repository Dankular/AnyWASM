$ErrorActionPreference = "Stop"

$source = @'
using System;
using System.Runtime.InteropServices;

public static class AnyWasmSystemParameters
{
    [DllImport("user32.dll", EntryPoint = "SystemParametersInfoW", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool Get(uint action, uint parameter, ref int value, uint flags);
}
'@
Add-Type $source

$cursorShadow = 0
if (-not [AnyWasmSystemParameters]::Get(0x101a, 0, [ref]$cursorShadow, 0)) {
    $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    throw "SystemParametersInfoW(SPI_GETCURSORSHADOW) failed with Win32 error $errorCode"
}

$mouseTrails = 0
if (-not [AnyWasmSystemParameters]::Get(0x005e, 0, [ref]$mouseTrails, 0)) {
    $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    throw "SystemParametersInfoW(SPI_GETMOUSETRAILS) failed with Win32 error $errorCode"
}

$mouseSonar = 0
if (-not [AnyWasmSystemParameters]::Get(0x101c, 0, [ref]$mouseSonar, 0)) {
    $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    throw "SystemParametersInfoW(SPI_GETMOUSESONAR) failed with Win32 error $errorCode"
}

$wheelScrollLines = 0
if (-not [AnyWasmSystemParameters]::Get(0x0068, 0, [ref]$wheelScrollLines, 0)) {
    $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    throw "SystemParametersInfoW(SPI_GETWHEELSCROLLLINES) failed with Win32 error $errorCode"
}

[pscustomobject]@{
    available = $true
    cursorShadow = ($cursorShadow -ne 0)
    mouseTrails = [uint32]$mouseTrails
    mouseSonar = ($mouseSonar -ne 0)
    wheelScrollLines = [uint32]$wheelScrollLines
} | ConvertTo-Json -Compress
