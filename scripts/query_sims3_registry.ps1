param(
    [Parameter(Mandatory = $true)]
    [string] $ExpectedInstallPath
)

$ErrorActionPreference = 'Stop'

$nativeSource = @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public sealed class AnyWasmRegistryKeyInfo
{
    public string ClassName;
    public uint SecurityDescriptorSize;
    public ulong LastWriteFileTime;
}

public sealed class AnyWasmRegistryValue
{
    public uint Type;
    public byte[] Data;
}

public static class AnyWasmRegistryNative
{
    [DllImport("advapi32.dll", EntryPoint = "RegQueryInfoKeyW", CharSet = CharSet.Unicode)]
    private static extern int RegQueryInfoKey(
        IntPtr key,
        StringBuilder keyClass,
        ref uint classLength,
        IntPtr reserved,
        out uint subKeys,
        out uint maxSubKeyLength,
        out uint maxClassLength,
        out uint values,
        out uint maxValueNameLength,
        out uint maxValueLength,
        out uint securityDescriptorLength,
        out long lastWriteFileTime);

    [DllImport("advapi32.dll", EntryPoint = "RegQueryValueExW", CharSet = CharSet.Unicode)]
    private static extern int RegQueryValueEx(
        IntPtr key,
        string valueName,
        IntPtr reserved,
        out uint type,
        byte[] data,
        ref uint dataLength);

    public static AnyWasmRegistryKeyInfo QueryKeyInfo(IntPtr key)
    {
        uint classLength = 32767;
        var keyClass = new StringBuilder((int)classLength);
        uint subKeys, maxSubKeyLength, maxClassLength, values;
        uint maxValueNameLength, maxValueLength, securityDescriptorLength;
        long lastWriteFileTime;
        int result = RegQueryInfoKey(
            key, keyClass, ref classLength, IntPtr.Zero,
            out subKeys, out maxSubKeyLength, out maxClassLength,
            out values, out maxValueNameLength, out maxValueLength,
            out securityDescriptorLength, out lastWriteFileTime);
        if (result != 0) throw new InvalidOperationException("RegQueryInfoKeyW failed with " + result);
        return new AnyWasmRegistryKeyInfo {
            ClassName = keyClass.ToString(),
            SecurityDescriptorSize = securityDescriptorLength,
            LastWriteFileTime = unchecked((ulong)lastWriteFileTime)
        };
    }

    public static AnyWasmRegistryValue QueryValue(IntPtr key, string name)
    {
        uint type;
        uint length = 0;
        int result = RegQueryValueEx(key, name, IntPtr.Zero, out type, null, ref length);
        if (result != 0) throw new InvalidOperationException("RegQueryValueExW size query failed with " + result);
        for (int attempt = 0; attempt < 4; attempt++)
        {
            byte[] data = new byte[length];
            uint actualLength = length;
            result = RegQueryValueEx(key, name, IntPtr.Zero, out type, data, ref actualLength);
            if (result == 234)
            {
                length = actualLength;
                continue;
            }
            if (result != 0) throw new InvalidOperationException("RegQueryValueExW failed with " + result);
            if (actualLength != data.Length) Array.Resize(ref data, (int)actualLength);
            return new AnyWasmRegistryValue { Type = type, Data = data };
        }
        throw new InvalidOperationException("Registry value changed repeatedly while being read");
    }
}
'@

function Normalize-InstallPath([string] $Path) {
    [System.IO.Path]::GetFullPath($Path).TrimEnd('\', '/')
}

function Get-RegistrySnapshot(
    [Microsoft.Win32.RegistryKey] $Root,
    [string] $Prefix,
    [System.Collections.Generic.List[object]] $Records,
    [bool] $IncludeSubkeys = $true
) {
    function Visit-Key([Microsoft.Win32.RegistryKey] $Key, [string] $Path) {
        $info = [AnyWasmRegistryNative]::QueryKeyInfo($Key.Handle.DangerousGetHandle())
        $subkeys = @($Key.GetSubKeyNames())
        $values = [System.Collections.Generic.List[object]]::new()
        foreach ($valueName in @($Key.GetValueNames())) {
            $raw = [AnyWasmRegistryNative]::QueryValue($Key.Handle.DangerousGetHandle(), [string]$valueName)
            $values.Add([ordered]@{
                name = [string]$valueName
                type = [uint32]$raw.Type
                data = [Convert]::ToBase64String($raw.Data)
            })
        }

        $Records.Add([ordered]@{
            path = $Path
            className = [string]$info.ClassName
            securityDescriptorSize = [uint32]$info.SecurityDescriptorSize
            lastWriteFiletime = [string]$info.LastWriteFileTime
            subkeys = @($subkeys)
            values = $values.ToArray()
        })

        if (-not $IncludeSubkeys) { return }
        foreach ($subkeyName in $subkeys) {
            $child = $Key.OpenSubKey($subkeyName, $false)
            if ($null -eq $child) {
                throw "Registry subkey disappeared while reading: $Path\$subkeyName"
            }
            try {
                Visit-Key $child "$Path\$subkeyName"
            } finally {
                $child.Dispose()
            }
        }
    }

    Visit-Key $Root $Prefix
}

try {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw 'host is not Windows'
    }
    Add-Type -TypeDefinition $nativeSource -Language CSharp

    $expectedPath = Normalize-InstallPath $ExpectedInstallPath
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
        [Microsoft.Win32.RegistryHive]::LocalMachine,
        [Microsoft.Win32.RegistryView]::Registry32)
    try {
        $root = $base.OpenSubKey('SOFTWARE\Sims(Steam)', $false)
        if ($null -eq $root) {
            throw 'HKLM\Software\Sims(Steam) is absent in the 32-bit registry view'
        }
        try {
            $registration = $root.OpenSubKey('The Sims 3', $false)
            if ($null -eq $registration) {
                throw 'HKLM\Software\Sims(Steam)\The Sims 3 is absent in the 32-bit registry view'
            }
            try {
                $registeredPath = $registration.GetValue('install dir', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            } finally {
                $registration.Dispose()
            }
            if (-not ($registeredPath -is [string]) -or (Normalize-InstallPath $registeredPath) -ine $expectedPath) {
                throw "Sims registry install dir does not match the active source path: $registeredPath"
            }
            $keys = [System.Collections.Generic.List[object]]::new()
            $missingKeys32 = [System.Collections.Generic.List[string]]::new()
            Get-RegistrySnapshot $root 'hkey_local_machine\software\sims(steam)' $keys
            $eaRoot = $base.OpenSubKey('SOFTWARE\Electronic Arts\Sims(Steam)', $false)
            if ($null -eq $eaRoot) {
                throw 'HKLM\Software\Electronic Arts\Sims(Steam) is absent in the 32-bit registry view'
            }
            try {
                Get-RegistrySnapshot $eaRoot 'hkey_local_machine\software\electronic arts\sims(steam)' $keys
            } finally {
                $eaRoot.Dispose()
            }
            $cpuRoot = $base.OpenSubKey('HARDWARE\DESCRIPTION\System\CentralProcessor\0', $false)
            if ($null -eq $cpuRoot) {
                throw 'HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0 is absent from the host snapshot'
            }
            try {
                Get-RegistrySnapshot $cpuRoot 'hkey_local_machine\hardware\description\system\centralprocessor\0' $keys
            } finally {
                $cpuRoot.Dispose()
            }
            # This exact key was requested by the translated San Andreas startup trace.
            # Record only a confirmed 32-bit-view absence; unknown host registry paths remain
            # untranslated and stop explicitly in the guest.
            $quickTime = $base.OpenSubKey('SOFTWARE\Apple Computer, Inc.\QuickTime', $false)
            if ($null -eq $quickTime) {
                $missingKeys32.Add('hkey_local_machine\software\apple computer, inc.\quicktime')
            } else {
                $quickTime.Dispose()
            }
            $userBase = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
                [Microsoft.Win32.RegistryHive]::CurrentUser,
                [Microsoft.Win32.RegistryView]::Registry32)
            try {
                # The GTA runtime requested this exact user key. Capture only
                # its tree when present; otherwise report the exact absence in
                # Registry32 so RegOpenKeyEx can return ERROR_FILE_NOT_FOUND.
                $userQuickTime = $userBase.OpenSubKey(
                    'Software\Apple Computer, Inc.\QuickTime',
                    $false)
                if ($null -eq $userQuickTime) {
                    $missingKeys32.Add('hkey_current_user\software\apple computer, inc.\quicktime')
                } else {
                    try {
                        Get-RegistrySnapshot $userQuickTime 'hkey_current_user\software\apple computer, inc.\quicktime' $keys
                    } finally {
                        $userQuickTime.Dispose()
                    }
                }

                $shellFolders = $userBase.OpenSubKey(
                    'Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders',
                    $false)
                if ($null -ne $shellFolders) {
                    try {
                        Get-RegistrySnapshot $shellFolders 'hkey_current_user\software\microsoft\windows\currentversion\explorer\shell folders' $keys $false
                    } finally {
                        $shellFolders.Dispose()
                    }
                }
            } finally {
                $userBase.Dispose()
            }
        } finally {
            $root.Dispose()
        }
    } finally {
        $base.Dispose()
    }

    [ordered]@{
        available = $true
        installPath = $expectedPath
        keys = $keys.ToArray()
        missingKeys32 = $missingKeys32.ToArray()
    } | ConvertTo-Json -Depth 12 -Compress
} catch {
    [ordered]@{
        available = $false
        reason = $_.Exception.Message
    } | ConvertTo-Json -Compress
}
