param(
    [Parameter(Mandatory = $true)][int]$RootProcessId,
    [string]$DataDirectory,
    [string]$Output,
    [int]$DurationSeconds = 0,
    [int]$IntervalSeconds = 60,
    [switch]$Detailed
)
$ErrorActionPreference = 'Stop'
if ($DurationSeconds -lt 0 -or $IntervalSeconds -lt 1 -or $IntervalSeconds -gt 60) { throw '采样时间无效' }
$root = Get-Process -Id $RootProcessId
if ($root.ProcessName -ne 'keencode-desktop') { throw '只允许读取指定 KeenCode 桌面及后代进程' }
$rootStarted = $root.StartTime.ToUniversalTime().ToString('o')
$cores = (Get-CimInstance Win32_ComputerSystem).NumberOfLogicalProcessors
if (-not ('KeenMemoryProbe.Native' -as [type])) {
    # 只读取 Windows 计数与地址区间元数据，不读取进程内容、不 trim、不暂停线程。
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace KeenMemoryProbe {
    [StructLayout(LayoutKind.Sequential)] public struct Counters {
        public uint cb, PageFaultCount;
        public UIntPtr PeakWorkingSetSize, WorkingSetSize, QuotaPeakPagedPoolUsage, QuotaPagedPoolUsage;
        public UIntPtr QuotaPeakNonPagedPoolUsage, QuotaNonPagedPoolUsage, PagefileUsage, PeakPagefileUsage;
        public UIntPtr PrivateUsage, PrivateWorkingSetSize;
        public ulong SharedCommitUsage;
    }
    [StructLayout(LayoutKind.Sequential)] public struct Region {
        public IntPtr BaseAddress, AllocationBase;
        public uint AllocationProtect;
        public ushort PartitionId;
        public UIntPtr RegionSize;
        public uint State, Protect, Type;
    }
    public class Usage {
        public bool Success;
        public int Error;
        public ulong WorkingSet, PrivateWorkingSet, PrivateCommit, SharedCommit, PageFaults;
        public ulong PrivateCommitted, MappedCommitted, ImageCommitted, Reserved;
        public uint RegionCount;
    }
    public static class Native {
        [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
        [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
        [DllImport("psapi.dll", SetLastError=true)] static extern bool GetProcessMemoryInfo(IntPtr handle, ref Counters counters, uint size);
        [DllImport("kernel32.dll", SetLastError=true)] static extern UIntPtr VirtualQueryEx(IntPtr handle, IntPtr address, out Region region, UIntPtr size);
        public static Usage Read(int pid, bool detailed) {
            Usage result = new Usage();
            IntPtr handle = OpenProcess(0x410, false, pid);
            if (handle == IntPtr.Zero) { result.Error = Marshal.GetLastWin32Error(); return result; }
            try {
                Counters counters = new Counters();
                counters.cb = (uint)Marshal.SizeOf(typeof(Counters));
                if (!GetProcessMemoryInfo(handle, ref counters, counters.cb)) { result.Error = Marshal.GetLastWin32Error(); return result; }
                result.Success = true;
                result.WorkingSet = counters.WorkingSetSize.ToUInt64();
                result.PrivateWorkingSet = counters.PrivateWorkingSetSize.ToUInt64();
                result.PrivateCommit = counters.PrivateUsage.ToUInt64();
                result.SharedCommit = counters.SharedCommitUsage;
                result.PageFaults = counters.PageFaultCount;
                if (detailed && IntPtr.Size == 8) {
                    ulong address = 0;
                    while (address < 0x0000800000000000UL) {
                        Region region;
                        if (VirtualQueryEx(handle, new IntPtr((long)address), out region, (UIntPtr)Marshal.SizeOf(typeof(Region))).ToUInt64() == 0) break;
                        ulong size = region.RegionSize.ToUInt64();
                        if (size == 0 || (ulong)region.BaseAddress.ToInt64() + size <= address) break;
                        if (region.State == 0x1000) {
                            result.RegionCount++;
                            if (region.Type == 0x20000) result.PrivateCommitted += size;
                            else if (region.Type == 0x40000) result.MappedCommitted += size;
                            else if (region.Type == 0x1000000) result.ImageCommitted += size;
                        } else if (region.State == 0x2000) result.Reserved += size;
                        address = (ulong)region.BaseAddress.ToInt64() + size;
                    }
                }
                return result;
            } finally { CloseHandle(handle); }
        }
    }
}
'@
}
function Read-OwnedMemory {
    $currentRoot = Get-Process -Id $RootProcessId -ErrorAction Stop
    if ($currentRoot.StartTime.ToUniversalTime().ToString('o') -ne $rootStarted) { throw '进程身份已变更，停止采样' }
    $catalog = @(Get-CimInstance Win32_Process)
    $ids = [System.Collections.Generic.HashSet[int]]::new()
    [void]$ids.Add($RootProcessId)
    do {
        $added = $false
        foreach ($entry in $catalog) {
            if ($ids.Contains([int]$entry.ParentProcessId) -and $ids.Add([int]$entry.ProcessId)) { $added = $true }
        }
    } while ($added)
    $rows = @(foreach ($entry in $catalog | Where-Object { $ids.Contains([int]$_.ProcessId) }) {
        $process = Get-Process -Id $entry.ProcessId -ErrorAction SilentlyContinue
        if (-not $process) { continue }
        $role = if ($entry.ProcessId -eq $RootProcessId) { 'desktop' } elseif ($entry.CommandLine -match '--type=([^\s]+)') { $Matches[1] } elseif ($process.ProcessName -like '*webview2*') { 'browser' } else { $process.ProcessName }
        $usage = [KeenMemoryProbe.Native]::Read($process.Id, $Detailed.IsPresent)
        [pscustomobject]@{
            pid = $process.Id; role = $role; cpuMs = $process.TotalProcessorTime.TotalMilliseconds
            threads = $process.Threads.Count; handles = $process.HandleCount; success = $usage.Success; error = $usage.Error
            rssBytes = $usage.WorkingSet; privateWorkingSetBytes = $usage.PrivateWorkingSet
            privateCommitBytes = $usage.PrivateCommit; sharedCommitBytes = $usage.SharedCommit; pageFaults = $usage.PageFaults
            vm = if ($Detailed) { @{ privateCommitted = $usage.PrivateCommitted; mappedCommitted = $usage.MappedCommitted; imageCommitted = $usage.ImageCommitted; reserved = $usage.Reserved; committedRegions = $usage.RegionCount } } else { $null }
        }
    })
    # GPU 专用/共享显存是独立计数，不能再叠加到 RSS 或 PrivateUsage。
    $gpu = @(Get-CimInstance Win32_PerfFormattedData_GPUPerformanceCounters_GPUProcessMemory -ErrorAction SilentlyContinue | ForEach-Object {
        if ($_.Name -match '^pid_(\d+)_' -and $ids.Contains([int]$Matches[1])) {
            [pscustomobject]@{ pid = [int]$Matches[1]; dedicatedBytes = $_.DedicatedUsage; sharedBytes = $_.SharedUsage; totalCommittedBytes = $_.TotalCommitted }
        }
    })
    $active = $null
    if ($DataDirectory) {
        $active = @(Get-ChildItem -LiteralPath (Join-Path $DataDirectory 'projects') -Filter metadata.json -Recurse -File | ForEach-Object {
            $metadata = (Get-Content -LiteralPath $_.FullName -Raw -Encoding UTF8 | ConvertFrom-Json).metadata
            if ($metadata.status -ne 'idle' -and -not $metadata.archived) { $metadata.status }
        }).Count
    }
    [pscustomobject]@{
        atUnixMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds(); rootPid = $RootProcessId; rootStartedUtc = $rootStarted
        logicalCores = $cores; responding = $currentRoot.Responding; activeSessions = $active; processes = $rows; gpu = $gpu
        rssBytes = ($rows | Measure-Object rssBytes -Sum).Sum
        privateWorkingSetBytes = ($rows | Measure-Object privateWorkingSetBytes -Sum).Sum
        privateCommitBytes = ($rows | Measure-Object privateCommitBytes -Sum).Sum
        sharedCommitBytes = ($rows | Measure-Object sharedCommitBytes -Sum).Sum
    }
}
if ($Output) {
    $Output = [System.IO.Path]::GetFullPath($Output)
    [void][System.IO.Directory]::CreateDirectory((Split-Path -Parent $Output))
    if (Test-Path -LiteralPath $Output) { throw '禁止覆盖既有采样证据' }
}
$started = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$previous = $null
do {
    $sample = Read-OwnedMemory
    $elapsed = if ($previous) { $sample.atUnixMs - $previous.atUnixMs } else { 0 }
    $cpuMs = 0.0
    if ($previous) {
        foreach ($row in $sample.processes) {
            $old = $previous.processes | Where-Object pid -EQ $row.pid | Select-Object -First 1
            if ($old) { $cpuMs += [Math]::Max(0.0, $row.cpuMs - $old.cpuMs) }
        }
    }
    $sample | Add-Member -NotePropertyName cpuPercentOfMachine -NotePropertyValue $(if ($elapsed -gt 0) { 100 * $cpuMs / $elapsed / $cores } else { $null })
    $line = $sample | ConvertTo-Json -Depth 7 -Compress
    if ($Output) { Add-Content -LiteralPath $Output -Value $line -Encoding utf8 } else { $line }
    $previous = $sample
    $remainingMs = $DurationSeconds * 1000 - ($sample.atUnixMs - $started)
    if ($remainingMs -le 0) { break }
    Start-Sleep -Milliseconds ([Math]::Min($IntervalSeconds * 1000, $remainingMs))
} while ($true)
if ($Output) { [pscustomobject]@{ output = $Output; elapsedSeconds = ($previous.atUnixMs - $started) / 1000 } | ConvertTo-Json -Compress }
