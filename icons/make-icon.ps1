# Draws the Set Creator icon: the GameShow mark (gold square, dark 2x2 board glyph) with a
# pencil badge. Writes icon.png (256 px, window icon) and icon.ico (16-256 px, exe icon).
# Run: powershell -ExecutionPolicy Bypass -File icons\make-icon.ps1
Add-Type -AssemblyName System.Drawing
$dir = $PSScriptRoot
$gold = [Drawing.Color]::FromArgb(255, 0xF5, 0xC5, 0x42)
$ink = [Drawing.Color]::FromArgb(255, 0x1A, 0x14, 0x00)
$navy = [Drawing.Color]::FromArgb(255, 0x1B, 0x2A, 0x8A)

function RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90); $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90); $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure(); $p
}

# Everything is drawn on a 256 grid and scaled, so every size matches.
function Draw([int]$size, [bool]$badge) {
    $bmp = New-Object Drawing.Bitmap $size, $size, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = "AntiAlias"; $g.PixelOffsetMode = "HighQuality"; $g.Clear([Drawing.Color]::Transparent)
    $g.ScaleTransform($size / 256, $size / 256)

    $g.FillPath((New-Object Drawing.SolidBrush $gold), (RoundRect 8 8 240 240 52))

    # Board glyph: four rounded tiles (shifted up-left when the badge takes the corner).
    $ox = if ($badge) { 52 } else { 66 }; $tile = 54; $gap = 16
    $inkBrush = New-Object Drawing.SolidBrush $ink
    foreach ($r in 0, 1) { foreach ($c in 0, 1) {
        $g.FillPath($inkBrush, (RoundRect ($ox + $c * ($tile + $gap)) ($ox + $r * ($tile + $gap)) $tile $tile 12))
    } }

    if ($badge) {
        # Navy disc with a gold ring cut into the square, so it reads at small sizes.
        $g.FillEllipse((New-Object Drawing.SolidBrush $gold), 124, 124, 132, 132)
        $g.FillEllipse((New-Object Drawing.SolidBrush $navy), 134, 134, 112, 112)
        # Pencil, pointing down-left.
        $state = $g.Save()
        $g.TranslateTransform(190, 190); $g.RotateTransform(45)
        $white = New-Object Drawing.SolidBrush ([Drawing.Color]::White)
        $g.FillRectangle($white, -11, -36, 22, 46)                                   # body
        $g.FillPath((New-Object Drawing.SolidBrush $gold), (RoundRect -11 -46 22 14 4)) # eraser
        $tip = New-Object Drawing.Drawing2D.GraphicsPath
        $tip.AddPolygon([Drawing.PointF[]]@((New-Object Drawing.PointF -11, 14), (New-Object Drawing.PointF 11, 14), (New-Object Drawing.PointF 0, 38)))
        $g.FillPath($white, $tip)
        $lead = New-Object Drawing.Drawing2D.GraphicsPath
        $lead.AddPolygon([Drawing.PointF[]]@((New-Object Drawing.PointF -4, 30), (New-Object Drawing.PointF 4, 30), (New-Object Drawing.PointF 0, 38)))
        $g.FillPath((New-Object Drawing.SolidBrush $ink), $lead)
        $g.Restore($state)
    }
    $g.Dispose(); $bmp
}

# The leading comma stops PowerShell from unrolling the byte array into separate objects.
function PngBytes($bmp) { $ms = New-Object IO.MemoryStream; $bmp.Save($ms, [Drawing.Imaging.ImageFormat]::Png); , $ms.ToArray() }

$big = Draw 256 $true; $big.Save("$dir\icon.png", [Drawing.Imaging.ImageFormat]::Png)

# ICO with PNG entries; the badge is dropped below 32 px where it would only be a smudge.
$sizes = 256, 128, 64, 48, 32, 24, 16
$images = New-Object 'System.Collections.Generic.List[byte[]]'
foreach ($s in $sizes) { $images.Add([byte[]](PngBytes (Draw $s ($s -ge 32)))) }
$ms = New-Object IO.MemoryStream; $w = New-Object IO.BinaryWriter $ms
$w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]; $dim = if ($s -ge 256) { 0 } else { $s }
    $w.Write([Byte]$dim); $w.Write([Byte]$dim); $w.Write([Byte]0); $w.Write([Byte]0)
    $w.Write([UInt16]1); $w.Write([UInt16]32); $w.Write([UInt32]$images[$i].Length); $w.Write([UInt32]$offset)
    $offset += $images[$i].Length
}
foreach ($img in $images) { $w.Write($img) }
$w.Flush(); [IO.File]::WriteAllBytes("$dir\icon.ico", $ms.ToArray())
"icon.png + icon.ico ($($sizes -join ', ') px) written"
