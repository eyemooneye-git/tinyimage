# Build assets/tinyimage.ico from vector shapes so the exe icon and the
# window icon can share exactly the same artwork.
#
# Run from anywhere:
#   powershell -ExecutionPolicy Bypass -File tools\make_icon.ps1

Add-Type -AssemblyName System.Drawing

$root = Split-Path $PSScriptRoot -Parent
$assets = Join-Path $root "assets"
New-Item -ItemType Directory -Force -Path $assets | Out-Null

function New-RoundedPath([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $path.AddArc($x, $y, $d, $d, 180, 90)
    $path.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $path.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $path.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    return $path
}

function Draw-Icon([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.ScaleTransform($size / 1024.0, $size / 1024.0)

    # Soft shadow keeps the artwork readable without adding an opaque background.
    $shadow = New-RoundedPath 174 238 650 650 82
    $shadowBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(38, 15, 23, 42))
    $g.FillPath($shadowBrush, $shadow)
    $shadowBrush.Dispose()
    $shadow.Dispose()

    # Warm white photo card.
    $card = New-RoundedPath 158 190 650 650 82
    $white = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 255, 252, 248))
    $g.FillPath($white, $card)

    # Sky panel.
    $sky = New-RoundedPath 218 250 530 330 48
    $skyGradient = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        (New-Object System.Drawing.RectangleF(218, 250, 530, 330)),
        [System.Drawing.Color]::FromArgb(255, 232, 246, 255),
        [System.Drawing.Color]::FromArgb(255, 190, 226, 220),
        [System.Drawing.Drawing2D.LinearGradientMode]::Vertical)
    $g.FillPath($skyGradient, $sky)

    # Sunset.
    $sunBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 255, 177, 92))
    $g.FillEllipse($sunBrush, 610, 298, 92, 92)
    $sunBrush.Dispose()

    # Mountains and a teal foreground.
    $farBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 71, 139, 139))
    $farMountain = @(
        (New-Object System.Drawing.PointF(218, 580)),
        (New-Object System.Drawing.PointF(370, 395)),
        (New-Object System.Drawing.PointF(500, 535)),
        (New-Object System.Drawing.PointF(610, 420)),
        (New-Object System.Drawing.PointF(748, 580))
    )
    $g.FillPolygon($farBrush, $farMountain)
    $farBrush.Dispose()

    $hillBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 36, 111, 109))
    $hill = @(
        (New-Object System.Drawing.PointF(218, 580)),
        (New-Object System.Drawing.PointF(390, 485)),
        (New-Object System.Drawing.PointF(520, 580))
    )
    $g.FillPolygon($hillBrush, $hill)
    $hillBrush.Dispose()
    $skyGradient.Dispose()
    $sky.Dispose()

    $white.Dispose()
    $card.Dispose()

    # Compression chevrons: a small, recognizable downward squeeze.
    $coral = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 245, 120, 95))
    $pen = New-Object System.Drawing.Pen($coral, 58)
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $g.DrawLines($pen, @(
        (New-Object System.Drawing.PointF(390, 640)),
        (New-Object System.Drawing.PointF(482, 732)),
        (New-Object System.Drawing.PointF(574, 640))
    ))
    $g.DrawLines($pen, @(
        (New-Object System.Drawing.PointF(390, 746)),
        (New-Object System.Drawing.PointF(482, 838)),
        (New-Object System.Drawing.PointF(574, 746))
    ))
    $pen.Dispose()
    $coral.Dispose()
    $g.Dispose()
    return $bmp
}

function Get-DibBytes([System.Drawing.Bitmap]$bmp) {
    $size = $bmp.Width
    $xor = New-Object byte[] ($size * $size * 4)
    for ($y = 0; $y -lt $size; $y++) {
        for ($x = 0; $x -lt $size; $x++) {
            $c = $bmp.GetPixel($x, $size - 1 - $y)
            $i = ($y * $size + $x) * 4
            $xor[$i] = $c.B
            $xor[$i + 1] = $c.G
            $xor[$i + 2] = $c.R
            $xor[$i + 3] = $c.A
        }
    }

    $maskRow = [int]([math]::Ceiling($size / 32.0) * 4)
    $mask = New-Object byte[] ($maskRow * $size)

    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter($ms)
    $bw.Write([uint32]40)
    $bw.Write([int32]$size)
    $bw.Write([int32]($size * 2))
    $bw.Write([uint16]1)
    $bw.Write([uint16]32)
    $bw.Write([uint32]0)
    $bw.Write([uint32]($xor.Length + $mask.Length))
    $bw.Write([int32]0)
    $bw.Write([int32]0)
    $bw.Write([uint32]0)
    $bw.Write([uint32]0)
    $bw.Write($xor)
    $bw.Write($mask)
    $bw.Flush()
    $result = $ms.ToArray()
    $bw.Dispose()
    $ms.Dispose()
    return ,$result
}

$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$images = @()

foreach ($size in $sizes) {
    $bmp = Draw-Icon $size
    # PNG entries keep the 128/256 px frames compact; smaller sizes stay as
    # classic DIBs for maximum compatibility in the title bar and Explorer.
    if ($size -ge 128) {
        $ms = New-Object System.IO.MemoryStream
        $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $bytes = $ms.ToArray()
        $ms.Dispose()
    } else {
        $bytes = Get-DibBytes $bmp
    }
    $bmp.Dispose()
    $images += [pscustomobject]@{ Size = $size; Bytes = $bytes }
}

$icoPath = Join-Path $assets "tinyimage.ico"
$out = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter($out)
$bw.Write([uint16]0)
$bw.Write([uint16]1)
$bw.Write([uint16]$images.Count)

$offset = 6 + 16 * $images.Count
foreach ($image in $images) {
    $dim = if ($image.Size -eq 256) { 0 } else { $image.Size }
    $bw.Write([byte]$dim)
    $bw.Write([byte]$dim)
    $bw.Write([byte]0)
    $bw.Write([byte]0)
    $bw.Write([uint16]1)
    $bw.Write([uint16]32)
    $bw.Write([uint32]$image.Bytes.Length)
    $bw.Write([uint32]$offset)
    $offset += $image.Bytes.Length
}
foreach ($image in $images) {
    $bw.Write([byte[]]$image.Bytes)
}
$bw.Flush()
[System.IO.File]::WriteAllBytes($icoPath, $out.ToArray())
$bw.Dispose()
$out.Dispose()

Write-Output "Wrote $icoPath ($((Get-Item $icoPath).Length) bytes)"
