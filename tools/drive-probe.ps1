# drive-probe.ps1 - replays, step by step, what the OLD peaDrive code did for each drive letter,
# so we can see exactly which Windows call hides a drive (default letter R:).
# Run (normal PowerShell, the same way you run peaDrive):
#   powershell -ExecutionPolicy Bypass -File .\drive-probe.ps1
#   powershell -ExecutionPolicy Bypass -File .\drive-probe.ps1 -Letter R
# Nothing is written or changed: it only READS drive information.
#
# The old code listed a drive only if fs4::total_space(<root>) succeeded. On Windows fs4 does, in order:
#   step 1  GetVolumePathNameW(root)
#   step 2  GetDiskFreeSpaceExW(volume root)
#   step 3  GetDiskFreeSpaceW(volume root)      <- legacy API
# and returns an error (=> drive HIDDEN) as soon as ANY of the three fails.
param([string]$Letter = 'R')
$ErrorActionPreference = 'Stop'

$src = @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class Probe {
    [DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);
    [DllImport("kernel32.dll")] public static extern uint GetLogicalDrives();
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] public static extern uint GetDriveTypeW(string root);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool GetVolumePathNameW(string fileName, StringBuilder volumePathName, uint bufferLength);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool GetDiskFreeSpaceExW(string dir, out ulong freeToCaller, out ulong total, out ulong totalFree);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool GetDiskFreeSpaceW(string root, out uint sectorsPerCluster, out uint bytesPerSector, out uint freeClusters, out uint totalClusters);
}
'@
Add-Type -TypeDefinition $src

[void][Probe]::SetErrorMode(1)   # SEM_FAILCRITICALERRORS: no "no disk in drive" popups

function Describe-Error([int]$code) {
    $msg = (New-Object System.ComponentModel.Win32Exception($code)).Message
    "FAILED, error $code ($msg)"
}

try {
    $admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
} catch { $admin = 'unknown' }
"Running elevated (administrator): $admin"
"Windows: $([Environment]::OSVersion.VersionString)"
""

$driveTypes = @{ 0 = 'UNKNOWN'; 1 = 'NO_ROOT_DIR'; 2 = 'REMOVABLE'; 3 = 'FIXED'; 4 = 'REMOTE'; 5 = 'CDROM'; 6 = 'RAMDISK' }
$mask = [Probe]::GetLogicalDrives()
$L = $Letter.Substring(0, 1).ToUpper()
$verdicts = @{}

foreach ($i in 0..25) {
    if (($mask -band (1 -shl $i)) -eq 0) { continue }
    $root = [string][char](65 + $i) + ':\'
    $type = $driveTypes[[int][Probe]::GetDriveTypeW($root)]
    "{0}   type={1}" -f $root, $type

    $failedStep = 0
    $sw = [System.Diagnostics.Stopwatch]::StartNew()

    # step 1: GetVolumePathNameW
    $sb = New-Object System.Text.StringBuilder 261
    if ([Probe]::GetVolumePathNameW($root, $sb, 261)) {
        $vol = $sb.ToString(); $r1 = "OK -> '$vol'"
    } else {
        $vol = $root; $r1 = Describe-Error ([Runtime.InteropServices.Marshal]::GetLastWin32Error()); if (-not $failedStep) { $failedStep = 1 }
    }
    $t1 = $sw.ElapsedMilliseconds; $sw.Restart()

    # step 2: GetDiskFreeSpaceExW (the only call the lean fix needs)
    $free = [uint64]0; $total = [uint64]0; $volFree = [uint64]0
    $exOk = [Probe]::GetDiskFreeSpaceExW($vol, [ref]$free, [ref]$total, [ref]$volFree)
    if ($exOk) {
        $r2 = 'OK, total ' + [math]::Round($total / 1GB, 2) + ' GB, free ' + [math]::Round($free / 1GB, 2) + ' GB'
    } else {
        $r2 = Describe-Error ([Runtime.InteropServices.Marshal]::GetLastWin32Error()); if (-not $failedStep) { $failedStep = 2 }
    }
    $t2 = $sw.ElapsedMilliseconds; $sw.Restart()

    # step 3: GetDiskFreeSpaceW (legacy)
    $spc = [uint32]0; $bps = [uint32]0; $fc = [uint32]0; $tc = [uint32]0
    if ([Probe]::GetDiskFreeSpaceW($vol, [ref]$spc, [ref]$bps, [ref]$fc, [ref]$tc)) {
        $r3 = "OK, cluster $([uint64]$spc * [uint64]$bps) bytes, $tc clusters"
    } else {
        $r3 = Describe-Error ([Runtime.InteropServices.Marshal]::GetLastWin32Error()); if (-not $failedStep) { $failedStep = 3 }
    }
    $t3 = $sw.ElapsedMilliseconds

    "  step 1  GetVolumePathNameW   : $r1   [$t1 ms]"
    "  step 2  GetDiskFreeSpaceExW  : $r2   [$t2 ms]"
    "  step 3  GetDiskFreeSpaceW    : $r3   [$t3 ms]"
    "  Test-Path: $(Test-Path -LiteralPath $root)"
    if ($failedStep) {
        $old = "OLD code (fs4::total_space) = ERROR at step $failedStep -> drive HIDDEN"
    } else {
        $old = "OLD code (fs4::total_space) = OK -> drive listed"
    }
    $new = if ($exOk) { "step-2-only code would list it WITH size" } else { "step-2-only code would NOT get a size (needs 'list anyway')" }
    "  => $old"
    "  => $new"
    ""
    $verdicts[$root] = "$old | $new"
}

$target = $L + ':\'
"=== SUMMARY for ${L}: ==="
if (-not ($mask -band (1 -shl ([int][char]$L - 65)))) {
    "Drive ${L}: is NOT returned by GetLogicalDrives for this process (drive letters can belong to one logon session; compare elevated vs normal)."
} else {
    "Drive ${L}: is returned by GetLogicalDrives."
    $verdicts[$target]
}
try {
    $d = Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='${L}:'"
    if ($d) {
        "WMI Win32_LogicalDisk ${L}: DriveType=$($d.DriveType) FileSystem=$($d.FileSystem) Size=$($d.Size) FreeSpace=$($d.FreeSpace)"
    } else {
        "WMI Win32_LogicalDisk ${L}: WMI does not report this drive"
    }
} catch { "CIM/WMI query failed: $($_.Exception.Message)" }
