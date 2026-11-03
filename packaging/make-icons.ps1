# Draws every icon file for the package, plus the exe's QuickEdit.ico. Run it again after any
# change here, and commit the results:
#
#   pwsh -NoProfile -File packaging\make-icons.ps1
#
# The art copies the Mac app's icon, QuickEdit/Resources/QuickEdit.icon: a pale page with a
# top-to-bottom gradient, three grey text rows that get shorter, faint ruled lines and a blue
# caret after the last row. Its numbers are written here, in the Mac file's 1024 unit grid after
# its 1.24272 scale, so this script needs no Mac files. Nothing here opens a window.
#
# Below 64 px the Mac layout cannot be read, so these sizes use a layout set on a 16 by 16 grid:
# no ruled lines, thicker rows, whole-pixel edges.
#
# Limits, compared with the Mac icon. Icon Composer's shadow under the caret and the system's
# Mac tile shape are not drawn. Windows 11's tile corners are used. The Mac's dark look is not
# used, because Windows has one colorful app icon for both themes here.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$assets = Join-Path $PSScriptRoot 'Assets'
New-Item -ItemType Directory -Force $assets | Out-Null

function Color([string]$hex) { [System.Drawing.ColorTranslator]::FromHtml($hex) }

function RoundedPath([double]$x, [double]$y, [double]$w, [double]$h, [double]$r) {
    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $d = [float](2 * $r)
    if ($d -le 0) {
        $path.AddRectangle([System.Drawing.RectangleF]::new($x, $y, $w, $h))
        return , $path
    }
    $path.AddArc([float]$x, [float]$y, $d, $d, 180, 90)
    $path.AddArc([float]($x + $w - $d), [float]$y, $d, $d, 270, 90)
    $path.AddArc([float]($x + $w - $d), [float]($y + $h - $d), $d, $d, 0, 90)
    $path.AddArc([float]$x, [float]($y + $h - $d), $d, $d, 90, 90)
    $path.CloseFigure()
    return , $path
}

function FillRounded($g, $brush, [double]$x, [double]$y, [double]$w, [double]$h, [double]$r) {
    $path = RoundedPath $x $y $w $h $r
    $g.FillPath($brush, $path)
    $path.Dispose()
}

# Draws the square icon with its top left at (x, y), `s` pixels wide.
function DrawArt($g, [double]$x, [double]$y, [double]$s) {
    $radius = [Math]::Max(3.0, $s * 0.19)
    $top = [System.Drawing.PointF]::new(0, [float]$y)
    $bottom = [System.Drawing.PointF]::new(0, [float]($y + $s))
    $page = [System.Drawing.Drawing2D.LinearGradientBrush]::new($top, $bottom, (Color '#ffffff'), (Color '#e9edf2'))
    FillRounded $g $page $x $y $s $s $radius
    $page.Dispose()

    $caretTop = [System.Drawing.PointF]::new(0, [float]($y + $s * 0.45))
    $caretBottom = [System.Drawing.PointF]::new(0, [float]($y + $s * 0.80))
    $caret = [System.Drawing.Drawing2D.LinearGradientBrush]::new($caretTop, $caretBottom, (Color '#2f8bff'), (Color '#0a64d8'))

    if ($s -ge 64) {
        # The Mac layout, in its 1024 grid.
        $u = $s / 1024.0
        $clip = RoundedPath $x $y $s $s $radius
        $g.SetClip($clip)
        $rule = [System.Drawing.SolidBrush]::new((Color '#d6dde6'))
        foreach ($ry in 308, 457, 606, 755, 904) {
            $g.FillRectangle($rule, [float]$x, [float]($y + $ry * $u), [float]$s, [float][Math]::Max(1.0, 10 * $u))
        }
        $rule.Dispose()
        $g.ResetClip()
        $clip.Dispose()
        $text = [System.Drawing.SolidBrush]::new((Color '#a3acb8'))
        FillRounded $g $text ($x + 132 * $u) ($y + 239 * $u) (701 * $u) (50 * $u) (25 * $u)
        FillRounded $g $text ($x + 132 * $u) ($y + 388 * $u) (539 * $u) (50 * $u) (25 * $u)
        FillRounded $g $text ($x + 132 * $u) ($y + 537 * $u) (278 * $u) (50 * $u) (25 * $u)
        $text.Dispose()
        FillRounded $g $caret ($x + 460 * $u) ($y + 465 * $u) (80 * $u) (211 * $u) (17 * $u)
        $edge = (Color '#c3cad4')
    }
    else {
        # The small layout. Rows sit on a 16 grid, rounded to whole pixels.
        $n = { param([double]$v) [Math]::Round($v * $s / 16.0) }
        $rowHeight = [Math]::Max(2, (& $n 2))
        $rowRadius = if ($s -ge 32) { $rowHeight / 2.0 } else { 0 }
        $text = [System.Drawing.SolidBrush]::new((Color '#677283'))
        $left = & $n 3
        FillRounded $g $text ($x + $left) ($y + (& $n 3)) ((& $n 13) - $left) $rowHeight $rowRadius
        FillRounded $g $text ($x + $left) ($y + (& $n 6)) ((& $n 11) - $left) $rowHeight $rowRadius
        FillRounded $g $text ($x + $left) ($y + (& $n 9)) ((& $n 7) - $left) $rowHeight $rowRadius
        $text.Dispose()
        $caretRadius = if ($s -ge 32) { $s / 32.0 } else { 0 }
        FillRounded $g $caret ($x + (& $n 8)) ($y + (& $n 8)) ([Math]::Max(2, (& $n 2))) ((& $n 13) - (& $n 8)) $caretRadius
        $edge = (Color '#aeb6c2')
    }
    $caret.Dispose()

    # A thin edge, so a white page still shows on a white taskbar or Start list.
    $width = [Math]::Max(1.0, $s / 128.0)
    $pen = [System.Drawing.Pen]::new($edge, [float]$width)
    $outline = RoundedPath ($x + $width / 2) ($y + $width / 2) ($s - $width) ($s - $width) ($radius - $width / 2)
    $g.DrawPath($pen, $outline)
    $outline.Dispose()
    $pen.Dispose()
}

# Writes one PNG, with the art `artSize` pixels wide in the middle of a clear w by h image.
function WritePng([string]$path, [int]$w, [int]$h, [int]$artSize) {
    $bitmap = [System.Drawing.Bitmap]::new($w, $h, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bitmap)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    DrawArt $g ([Math]::Floor(($w - $artSize) / 2)) ([Math]::Floor(($h - $artSize) / 2)) $artSize
    $g.Dispose()
    $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
}

# Old files go first, so a size removed here does not stay in the package.
Get-ChildItem $assets -Filter '*.png' | Remove-Item -Force

$scales = 100, 125, 150, 200, 400
$rounded = { param([double]$v) [int][Math]::Round($v, [MidpointRounding]::AwayFromZero) }

# name, base width, base height, how much of the shorter side the art fills.
$tiles = @(
    @('Square44x44Logo', 44, 44, 0.86),
    @('Square71x71Logo', 71, 71, 0.70),
    @('Square150x150Logo', 150, 150, 0.60),
    @('Wide310x150Logo', 310, 150, 0.60),
    @('Square310x310Logo', 310, 310, 0.60),
    @('StoreLogo', 50, 50, 1.00)
)
foreach ($tile in $tiles) {
    foreach ($scale in $scales) {
        $w = & $rounded ($tile[1] * $scale / 100.0)
        $h = & $rounded ($tile[2] * $scale / 100.0)
        $art = [int][Math]::Round([Math]::Min($w, $h) * $tile[3])
        WritePng (Join-Path $assets "$($tile[0]).scale-$scale.png") $w $h $art
    }
}

# Exact sizes for the taskbar, Start, Explorer and the title bar. "unplated" and "lightunplated"
# are the dark and light theme files. Windows draws a plate behind the icon without them.
$targets = 16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256
foreach ($size in $targets) {
    foreach ($suffix in '', '_altform-unplated', '_altform-lightunplated') {
        WritePng (Join-Path $assets "Square44x44Logo.targetsize-$size$suffix.png") $size $size $size
    }
}

# QuickEdit.ico sits beside Assets, not in it, because only the exe needs it. Each entry is a
# PNG, which Windows reads from Vista on.
$icoSizes = 16, 20, 24, 32, 40, 48, 64, 256
$entries = foreach ($size in $icoSizes) {
    , [System.IO.File]::ReadAllBytes((Join-Path $assets "Square44x44Logo.targetsize-$size.png"))
}
$stream = [System.IO.MemoryStream]::new()
$writer = [System.IO.BinaryWriter]::new($stream)
$writer.Write([uint16]0)
$writer.Write([uint16]1)
$writer.Write([uint16]$icoSizes.Count)
$offset = 6 + 16 * $icoSizes.Count
for ($i = 0; $i -lt $icoSizes.Count; $i++) {
    $side = if ($icoSizes[$i] -eq 256) { 0 } else { $icoSizes[$i] }
    $writer.Write([byte]$side)
    $writer.Write([byte]$side)
    $writer.Write([byte]0)
    $writer.Write([byte]0)
    $writer.Write([uint16]1)
    $writer.Write([uint16]32)
    $writer.Write([uint32]$entries[$i].Length)
    $writer.Write([uint32]$offset)
    $offset += $entries[$i].Length
}
foreach ($entry in $entries) { $writer.Write([byte[]]$entry) }
$writer.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $PSScriptRoot 'QuickEdit.ico'), $stream.ToArray())
$writer.Dispose()

$files = Get-ChildItem $assets -Filter '*.png'
$bytes = ($files | Measure-Object Length -Sum).Sum
"Wrote $($files.Count) PNG files, $bytes bytes, and QuickEdit.ico."
