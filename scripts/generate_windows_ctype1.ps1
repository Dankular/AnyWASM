param(
    [string]$Output = (Join-Path $PSScriptRoot "..\crates\anywasm-rt\src\hle\windows_ctype1.bin")
)

$ErrorActionPreference = "Stop"

$source = @'
using System;
using System.Runtime.InteropServices;

public static class AnyWasmWindowsNls
{
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool GetStringTypeW(
        uint infoType,
        [In] char[] source,
        int sourceCount,
        [Out] ushort[] characterTypes);
}
'@
Add-Type $source

$characters = [char[]]::new(65536)
for ($i = 0; $i -lt $characters.Length; $i++) {
    $characters[$i] = [char]$i
}

$types = [ushort[]]::new($characters.Length)
if (-not [AnyWasmWindowsNls]::GetStringTypeW(1, $characters, $characters.Length, $types)) {
    $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    throw "GetStringTypeW(CT_CTYPE1) failed with Win32 error $errorCode"
}

$bytes = [byte[]]::new($types.Length * 2)
[Buffer]::BlockCopy($types, 0, $bytes, 0, $bytes.Length)
$fullOutput = [IO.Path]::GetFullPath($Output)
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($fullOutput)) | Out-Null
[IO.File]::WriteAllBytes($fullOutput, $bytes)

$hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
$os = [Environment]::OSVersion.Version.ToString()
$manifest = @(
    "source=kernel32!GetStringTypeW(CT_CTYPE1)"
    "code_units=65536"
    "bytes=$($bytes.Length)"
    "os_version=$os"
    "culture=$([Globalization.CultureInfo]::CurrentCulture.Name)"
    "sha256=$hash"
)
$manifestPath = [IO.Path]::ChangeExtension($fullOutput, ".txt")
[IO.File]::WriteAllLines($manifestPath, $manifest, [Text.UTF8Encoding]::new($false))
"Wrote $($bytes.Length) bytes to $fullOutput"
"NLS profile: $os / $([Globalization.CultureInfo]::CurrentCulture.Name)"
"SHA-256: $hash"
