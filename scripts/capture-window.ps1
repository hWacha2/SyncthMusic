# Снимок окна приложения через Win32 PrintWindow (без внешних зависимостей).
# Запуск: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\capture-window.ps1 -ProcessName syncth-desktop -Out shot.png
param(
    [string]$ProcessName = 'syncth-desktop',
    [string]$Out = 'window.png'
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

Add-Type -ReferencedAssemblies System.Drawing @'
using System;
using System.Runtime.InteropServices;
public static class WinCap {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int cmd);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr hwnd, int attr, out RECT value, int size);

    public static bool Capture(IntPtr hwnd, string path) {
        RECT r;
        const int DWMWA_EXTENDED_FRAME_BOUNDS = 9;
        if (DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, out r, Marshal.SizeOf(typeof(RECT))) != 0) {
            if (!GetWindowRect(hwnd, out r)) return false;
        }
        int w = r.Right - r.Left;
        int h = r.Bottom - r.Top;
        if (w <= 0 || h <= 0) return false;
        using (var bmp = new System.Drawing.Bitmap(w, h)) {
            using (var g = System.Drawing.Graphics.FromImage(bmp)) {
                IntPtr hdc = g.GetHdc();
                bool ok = PrintWindow(hwnd, hdc, 2); // PW_RENDERFULLCONTENT
                g.ReleaseHdc(hdc);
                if (!ok) {
                    g.CopyFromScreen(r.Left, r.Top, 0, 0, new System.Drawing.Size(w, h));
                }
            }
            bmp.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
        return true;
    }
}
'@

$proc = Get-Process -Name $ProcessName -ErrorAction Stop | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $proc) { throw "окно $ProcessName не найдено" }

$hwnd = $proc.MainWindowHandle
[WinCap]::ShowWindow($hwnd, 9) | Out-Null   # SW_RESTORE
[WinCap]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 500

$full = (Resolve-Path -LiteralPath (Split-Path -Parent $Out) -ErrorAction SilentlyContinue)
$path = if ($full) { Join-Path $full.Path (Split-Path -Leaf $Out) } else { $Out }
if ([WinCap]::Capture($hwnd, $path)) {
    Write-Host "снимок: $path"
} else {
    throw 'не удалось снять окно'
}
