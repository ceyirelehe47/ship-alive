param(
    [string]$Title = "Ship Alive",
    [string]$OutPath = "shot.png",
    [int]$WaitMs = 8000
)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Cap {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, int flags);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
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
[Win32Cap]::ShowWindow($h, 9) | Out-Null
[Win32Cap]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 800
$rect = New-Object Win32Cap+RECT
[Win32Cap]::GetWindowRect($h, [ref]$rect) | Out-Null
$w = $rect.Right - $rect.Left
$hh = $rect.Bottom - $rect.Top
if ($w -le 0 -or $hh -le 0) { Write-Output "bad rect"; exit 1 }
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap($w, $hh)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
[Win32Cap]::PrintWindow($h, $hdc, 2) | Out-Null
$g.ReleaseHdc($hdc)
$bmp.Save($OutPath, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "saved $OutPath (${w}x${hh})"
