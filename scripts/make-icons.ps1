# Генерация иконок приложения без внешних зависимостей (System.Drawing).
# Запуск: powershell -ExecutionPolicy Bypass -File scripts\make-icons.ps1

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$out = Join-Path (Split-Path $PSScriptRoot -Parent) 'crates\desktop\src-tauri\icons'
New-Item -ItemType Directory -Force $out | Out-Null

function New-Mark([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.ScaleTransform($size / 256.0, $size / 256.0)

    # Скруглённый фон «ночного пульта».
    $bg = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r = 56
    $bg.AddArc(0, 0, $r, $r, 180, 90)
    $bg.AddArc(256 - $r, 0, $r, $r, 270, 90)
    $bg.AddArc(256 - $r, 256 - $r, $r, $r, 0, 90)
    $bg.AddArc(0, 256 - $r, $r, $r, 90, 90)
    $bg.CloseFigure()
    $bgBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        (New-Object System.Drawing.Point 0, 0),
        (New-Object System.Drawing.Point 256, 256),
        [System.Drawing.Color]::FromArgb(255, 30, 23, 33),
        [System.Drawing.Color]::FromArgb(255, 18, 14, 21))
    $g.FillPath($bgBrush, $bg)

    $ember = [System.Drawing.Color]::FromArgb(255, 255, 106, 61)
    $lime = [System.Drawing.Color]::FromArgb(255, 217, 242, 107)
    $groove = [System.Drawing.Color]::FromArgb(255, 82, 68, 92)

    # Пластинка.
    $recordBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 15, 11, 17))
    $g.FillEllipse($recordBrush, 30, 46, 152, 152)
    $ring = New-Object System.Drawing.Pen $ember, 7
    $g.DrawEllipse($ring, 30, 46, 152, 152)

    $groovePen = New-Object System.Drawing.Pen $groove, 2.5
    foreach ($d in 116, 96, 76) {
        $off = (152 - $d) / 2
        $g.DrawEllipse($groovePen, 30 + $off, 46 + $off, $d, $d)
    }

    $labelBrush = New-Object System.Drawing.SolidBrush $ember
    $g.FillEllipse($labelBrush, 90, 106, 32, 32)
    $holeBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 18, 14, 21))
    $g.FillEllipse($holeBrush, 103, 119, 6, 6)

    # «Сигнал»: две дуги справа.
    $arcPen = New-Object System.Drawing.Pen $lime, 8
    $arcPen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $arcPen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $g.DrawArc($arcPen, 168, 100, 44, 44, -54, 108)
    $g.DrawArc($arcPen, 148, 80, 84, 84, -46, 92)

    $g.Dispose()
    $bmp
}

function Save-Png([System.Drawing.Bitmap]$bmp, [string]$path) {
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
}

foreach ($s in 32, 128, 256, 512) {
    $bmp = New-Mark $s
    $name = switch ($s) {
        32 { '32x32.png' }
        128 { '128x128.png' }
        256 { '128x128@2x.png' }
        512 { 'icon.png' }
    }
    Save-Png $bmp (Join-Path $out $name)
    $bmp.Dispose()
}

# ICO с одним 32-битным BMP-слоем 256x256.
function Write-Ico([string]$path) {
    $size = 256
    $bmp = New-Mark $size
    $rect = New-Object System.Drawing.Rectangle 0, 0, $size, $size
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $stride = $data.Stride
    $raw = New-Object byte[] ($stride * $size)
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $raw, 0, $raw.Length)
    $bmp.UnlockBits($data)
    $bmp.Dispose()

    $pixels = New-Object byte[] ($size * $size * 4)
    for ($y = 0; $y -lt $size; $y++) {
        $srcRow = ($size - 1 - $y) * $stride
        for ($x = 0; $x -lt $size; $x++) {
            $si = $srcRow + $x * 4
            $di = ($y * $size + $x) * 4
            $pixels[$di] = $raw[$si]
            $pixels[$di + 1] = $raw[$si + 1]
            $pixels[$di + 2] = $raw[$si + 2]
            $pixels[$di + 3] = $raw[$si + 3]
        }
    }
    $maskStride = 32
    $mask = New-Object byte[] ($maskStride * $size)
    $imageSize = 40 + $pixels.Length + $mask.Length

    $fs = [System.IO.File]::Create($path)
    $w = New-Object System.IO.BinaryWriter $fs
    $w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]1)
    $w.Write([Byte]0); $w.Write([Byte]0)          # 256 = 0
    $w.Write([Byte]0); $w.Write([Byte]0)
    $w.Write([UInt16]1); $w.Write([UInt16]32)
    $w.Write([UInt32]$imageSize)
    $w.Write([UInt32]22)
    $w.Write([UInt32]40)
    $w.Write([Int32]$size)
    $w.Write([Int32]($size * 2))
    $w.Write([UInt16]1); $w.Write([UInt16]32)
    $w.Write([UInt32]0)
    $w.Write([UInt32]$pixels.Length)
    $w.Write([Int32]0); $w.Write([Int32]0)
    $w.Write([UInt32]0); $w.Write([UInt32]0)
    $w.Write($pixels)
    $w.Write($mask)
    $w.Flush(); $w.Dispose(); $fs.Dispose()
}

Write-Ico (Join-Path $out 'icon.ico')
Write-Host "Иконки: $out"
