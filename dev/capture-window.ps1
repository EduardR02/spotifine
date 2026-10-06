# Captures a window at its true physical pixel size and writes a lossless PNG.
#
# Snipping Tool's editor shows the capture scaled to fit its own window, so a
# shot taken at 150% display scaling looks soft there even when the saved file
# is sharp. This avoids the question entirely: it marks itself DPI-aware, asks
# Windows for the window's real device pixels, and encodes straight to PNG.
#
#   .\dev\capture-window.ps1 -Process Spotifine -Out docs\raw-library.png
param(
  [string]$Process = "Spotifine",
  [Parameter(Mandatory = $true)][string]$Out
)

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@

[void][Win]::SetProcessDPIAware()

$proc = Get-Process -Name $Process -ErrorAction SilentlyContinue |
  Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $proc) { throw "no visible window for process '$Process'" }

[void][Win]::SetForegroundWindow($proc.MainWindowHandle)
Start-Sleep -Milliseconds 400   # let the window finish coming forward

$r = New-Object Win+RECT
[void][Win]::GetWindowRect($proc.MainWindowHandle, [ref]$r)
$w = $r.R - $r.L
$h = $r.B - $r.T

$bmp = New-Object System.Drawing.Bitmap $w, $h
$gfx = [System.Drawing.Graphics]::FromImage($bmp)
# CopyFromScreen rather than PrintWindow: this window is GPU-composited, and
# PrintWindow tends to come back black for those.
$gfx.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
$gfx.Dispose()

$path = if ([System.IO.Path]::IsPathRooted($Out)) { $Out }
        else { [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Out)) }
$dir = Split-Path -Parent $path
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }
$bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "$path  ${w}x${h}"
