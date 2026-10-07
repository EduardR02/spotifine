# Read-only, non-admin measurements for the running app and its subprocesses.
# CPU time is integrated over the interval, not rounded instantaneous usage.
# I/O includes pipes/network as well as files; working sets include shared pages.
[CmdletBinding()]
param(
  [ValidateRange(1, 3600)][int]$Seconds = 60,
  [ValidateRange(100, 10000)][int]$IntervalMilliseconds = 1000,
  [ValidateRange(0, 2147483647)][int]$ProcessId = 0,
  [switch]$Json
)

$ErrorActionPreference = 'Stop'
if (-not ('SpotifineProfilerNative' -as [type])) {
  Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class SpotifineProfilerNative {
  [StructLayout(LayoutKind.Sequential)]
  public struct IoCounters {
    public ulong ReadOperations, WriteOperations, OtherOperations;
    public ulong ReadBytes, WriteBytes, OtherBytes;
  }
  [DllImport("kernel32.dll", SetLastError = true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool GetProcessIoCounters(IntPtr process, out IoCounters counters);
  [DllImport("kernel32.dll", SetLastError = true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool QueryProcessCycleTime(IntPtr process, out ulong cycles);
  [StructLayout(LayoutKind.Sequential)]
  public struct Rect { public int Left, Top, Right, Bottom; }
  [DllImport("dwmapi.dll")]
  public static extern int DwmGetWindowAttribute(IntPtr window, int attribute, out Rect rect, int size);
  [DllImport("user32.dll")]
  public static extern uint GetDpiForWindow(IntPtr window);
  [DllImport("user32.dll")]
  public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool IsIconic(IntPtr window);
  [DllImport("user32.dll")]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool IsWindowVisible(IntPtr window);
}
'@
}

$all = @(Get-CimInstance Win32_Process -Filter "Name='Spotifine.exe' OR Name='PlaybackEngine.exe' OR Name='msedgewebview2.exe'")
$roots = @($all | Where-Object { $_.Name -eq 'Spotifine.exe' -and ($ProcessId -eq 0 -or $_.ProcessId -eq $ProcessId) })
if ($roots.Count -ne 1) {
  throw "Expected one running Spotifine process; found $($roots.Count). Start the app, or select it with -ProcessId."
}
$tree = @{}
$tree[[int]$roots[0].ProcessId] = $roots[0]
do {
  $added = $false
  foreach ($item in $all) {
    $id = [int]$item.ProcessId
    if (-not $tree.ContainsKey($id) -and $tree.ContainsKey([int]$item.ParentProcessId)) {
      $tree[$id] = $item
      $added = $true
    }
  }
} while ($added)

function Read-Snapshot($Process) {
  $Process.Refresh()
  if ($Process.HasExited) { throw "Process $($Process.Id) exited during capture; rerun against the new process tree." }
  $io = New-Object SpotifineProfilerNative+IoCounters
  [uint64]$cycles = 0
  if (-not [SpotifineProfilerNative]::GetProcessIoCounters($Process.Handle, [ref]$io) -or
      -not [SpotifineProfilerNative]::QueryProcessCycleTime($Process.Handle, [ref]$cycles)) {
    throw "Cannot read native counters for process $($Process.Id): Win32 error $([Runtime.InteropServices.Marshal]::GetLastWin32Error())."
  }
  [pscustomobject]@{
    CpuMs = $Process.TotalProcessorTime.TotalMilliseconds
    Cycles = $cycles
    WorkingSet = $Process.WorkingSet64
    PrivateBytes = $Process.PrivateMemorySize64
    ReadBytes = $io.ReadBytes
    WriteBytes = $io.WriteBytes
    OtherBytes = $io.OtherBytes
  }
}

function Read-WindowState($Process) {
  $Process.Refresh()
  $window = $Process.MainWindowHandle
  if ($window -eq [IntPtr]::Zero) { return $null }
  $rect = New-Object SpotifineProfilerNative+Rect
  $result = [SpotifineProfilerNative]::DwmGetWindowAttribute($window, 9, [ref]$rect, 16)
  [pscustomobject]@{
    WidthPhysicalPixels = if ($result -eq 0) { $rect.Right - $rect.Left } else { $null }
    HeightPhysicalPixels = if ($result -eq 0) { $rect.Bottom - $rect.Top } else { $null }
    BoundsHResult = $result
    Dpi = [SpotifineProfilerNative]::GetDpiForWindow($window)
    Minimized = [SpotifineProfilerNative]::IsIconic($window)
    Visible = [SpotifineProfilerNative]::IsWindowVisible($window)
    Foreground = [SpotifineProfilerNative]::GetForegroundWindow() -eq $window
  }
}

$entries = @()
try {
  foreach ($item in ($tree.Values | Sort-Object ProcessId)) {
    $process = [Diagnostics.Process]::GetProcessById([int]$item.ProcessId)
    $entry = [pscustomobject]@{
      Process = $process; Role = $item.Name; First = $null; Last = $null
      PeakWorkingSet = 0L; PeakPrivateBytes = 0L
    }
    $entries += $entry
    if ($item.Name -eq 'msedgewebview2.exe') {
      $entry.Role = if ($item.CommandLine -match '--type=([^ ]+)') { "webview/$($matches[1])" } else { 'webview/browser' }
    }
    $entry.First = Read-Snapshot $process
    $entry.Last = $entry.First
    $entry.PeakWorkingSet = $entry.First.WorkingSet
    $entry.PeakPrivateBytes = $entry.First.PrivateBytes
  }
  $rootProcess = ($entries | Where-Object { $_.Process.Id -eq $roots[0].ProcessId }).Process
  $windowAtStart = Read-WindowState $rootProcess
  $clock = [Diagnostics.Stopwatch]::StartNew()
  [long]$peakTotalWorkingSet = 0
  [long]$peakTotalPrivateBytes = 0
  do {
    $remaining = $Seconds * 1000 - $clock.Elapsed.TotalMilliseconds
    if ($remaining -gt 0) { Start-Sleep -Milliseconds ([int][Math]::Min($IntervalMilliseconds, [Math]::Ceiling($remaining))) }
    [long]$workingSet = 0
    [long]$privateBytes = 0
    foreach ($entry in $entries) {
      $entry.Last = Read-Snapshot $entry.Process
      $entry.PeakWorkingSet = [Math]::Max($entry.PeakWorkingSet, $entry.Last.WorkingSet)
      $entry.PeakPrivateBytes = [Math]::Max($entry.PeakPrivateBytes, $entry.Last.PrivateBytes)
      $workingSet += $entry.Last.WorkingSet
      $privateBytes += $entry.Last.PrivateBytes
    }
    $peakTotalWorkingSet = [Math]::Max($peakTotalWorkingSet, $workingSet)
    $peakTotalPrivateBytes = [Math]::Max($peakTotalPrivateBytes, $privateBytes)
  } while ($clock.Elapsed.TotalSeconds -lt $Seconds)
  $elapsed = $clock.Elapsed.TotalSeconds
  $logicalCpus = [Environment]::ProcessorCount
  $rows = @($entries | ForEach-Object {
    $cpuMs = $_.Last.CpuMs - $_.First.CpuMs
    [pscustomobject]@{
      ProcessId = $_.Process.Id
      Role = $_.Role
      CpuMilliseconds = [Math]::Round($cpuMs, 3)
      CpuMsPerSecond = [Math]::Round($cpuMs / $elapsed, 3)
      CpuPercentMachine = [Math]::Round($cpuMs / ($elapsed * 10 * $logicalCpus), 5)
      CpuCycles = $_.Last.Cycles - $_.First.Cycles
      PeakWorkingSetMiB = [Math]::Round($_.PeakWorkingSet / 1MB, 2)
      PeakPrivateCommitMiB = [Math]::Round($_.PeakPrivateBytes / 1MB, 2)
      ReadBytes = $_.Last.ReadBytes - $_.First.ReadBytes
      WriteBytes = $_.Last.WriteBytes - $_.First.WriteBytes
      OtherIoBytes = $_.Last.OtherBytes - $_.First.OtherBytes
    }
  })
  $report = [pscustomobject]@{
    RootProcessId = [int]$roots[0].ProcessId
    ElapsedSeconds = [Math]::Round($elapsed, 3)
    LogicalCpus = $logicalCpus
    WindowAtStart = $windowAtStart
    WindowAtEnd = Read-WindowState $rootProcess
    TotalCpuMsPerSecond = [Math]::Round(($rows | Measure-Object CpuMilliseconds -Sum).Sum / $elapsed, 3)
    PeakSummedWorkingSetMiB = [Math]::Round($peakTotalWorkingSet / 1MB, 2)
    PeakPrivateCommitMiB = [Math]::Round($peakTotalPrivateBytes / 1MB, 2)
    Notes = 'Tree captured at start; new subprocesses are excluded. Working sets include shared pages; private commit is not resident RAM. I/O includes pipes and network, not just disk. Sampling is passive: playback/window state was not changed.'
    Processes = $rows
  }
  if ($Json) { $report | ConvertTo-Json -Depth 4 }
  else {
    $report | Select-Object RootProcessId,ElapsedSeconds,LogicalCpus,TotalCpuMsPerSecond,PeakSummedWorkingSetMiB,PeakPrivateCommitMiB | Format-List
    Write-Output 'Window state at start/end (physical bounds; not a history of intermediate changes):'
    @($report.WindowAtStart, $report.WindowAtEnd) | Format-Table -AutoSize
    $rows | Format-Table ProcessId,Role,CpuMsPerSecond,CpuPercentMachine,PeakWorkingSetMiB,PeakPrivateCommitMiB -AutoSize
    $rows | Format-Table ProcessId,Role,CpuCycles,ReadBytes,WriteBytes,OtherIoBytes -AutoSize
    Write-Output $report.Notes
  }
} finally {
  foreach ($entry in $entries) { $entry.Process.Dispose() }
}
