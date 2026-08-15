param(
    [string]$Title = "Ship Alive",
    [string]$OutPath = "shot.png",
    [int]$WaitMs = 15000
)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Cap2 {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);
    public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
$deadline = (Get-Date).AddMilliseconds($WaitMs)
$proc = $null
while ((Get-Date) -lt $deadline) {
    $proc = Get-Process | Where-Object { $_.MainWindowTitle -like "*$Title*" } | Select-Object -First 1
    if ($proc) { break }
    Start-Sleep -Milliseconds 200
}
if (-not $proc) { Write-Output "window not found"; exit 1 }
$h = $proc.MainWindowHandle
[Win32Cap2]::ShowWindow($h, 9) | Out-Null
$topmost = [IntPtr](-1)
[Win32Cap2]::SetWindowPos($h, $topmost, 20, 20, 1440, 860, 0x0000) | Out-Null
Start-Sleep -Milliseconds 200
[Win32Cap2]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 900
$rect = New-Object Win32Cap2+RECT
[Win32Cap2]::GetWindowRect($h, [ref]$rect) | Out-Null
$w = $rect.Right - $rect.Left
$hh = $rect.Bottom - $rect.Top
if ($w -le 0 -or $hh -le 0) { Write-Output "bad rect"; exit 1 }
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap($w, $hh)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size($w, $hh)))
$bmp.Save($OutPath, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "saved $OutPath (${w}x${hh})"
