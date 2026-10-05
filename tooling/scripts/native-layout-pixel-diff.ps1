param(
    [Parameter(Mandatory = $true)]
    [string]$TargetDirectory,
    [string]$SourceDirectory = (Join-Path $PSScriptRoot '..\..\out\native-live\zcode-source-baseline-29628c9-dpr2')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

# 使用同一批次报告绑定实际二进制与截图，避免把历史截图写成当前构建的证据。
$outputDirectory = [IO.Path]::GetFullPath($TargetDirectory)
$sourcePath = Join-Path $SourceDirectory 'source-appearance-dpr1.png'
$sourceLayoutPath = Join-Path $SourceDirectory 'source-appearance-layout-dpr1.json'
$sourceAppearancePath = Join-Path $SourceDirectory 'source-appearance-dpr1.json'
$targetPath = Join-Path $outputDirectory '01-settings-zai-dark.png'
$targetLayoutPath = Join-Path $outputDirectory 'appearance-layout.json'
$targetReportPath = Join-Path $outputDirectory 'report.json'

function Get-IntRect($card) {
    [pscustomobject]@{
        Left = [int][Math]::Floor([double]$card.x)
        Top = [int][Math]::Floor([double]$card.y)
        Right = [int][Math]::Ceiling(([double]$card.x) + ([double]$card.width)) - 1
        Bottom = [int][Math]::Ceiling(([double]$card.y) + ([double]$card.height)) - 1
    }
}

function Compare-Region($sourceBitmap, $targetBitmap, $left, $top, $right, $bottom, $dx, $dy, $mode, $diffBitmap) {
    $pixels = 0
    $changed = 0
    [double]$absolute = 0
    $maxChannel = 0

    for ($y = $top; $y -le $bottom; $y++) {
        for ($x = $left; $x -le $right; $x++) {
            $targetX = $x - $dx
            $targetY = $y - $dy
            if ($x -lt 0 -or $y -lt 0 -or $x -ge $sourceBitmap.Width -or $y -ge $sourceBitmap.Height -or
                $targetX -lt 0 -or $targetY -lt 0 -or $targetX -ge $targetBitmap.Width -or $targetY -ge $targetBitmap.Height) {
                continue
            }

            $sourcePixel = $sourceBitmap.GetPixel($x, $y)
            $targetPixel = $targetBitmap.GetPixel($targetX, $targetY)
            $red = [Math]::Abs($sourcePixel.R - $targetPixel.R)
            $green = [Math]::Abs($sourcePixel.G - $targetPixel.G)
            $blue = [Math]::Abs($sourcePixel.B - $targetPixel.B)
            $sum = $red + $green + $blue

            $pixels++
            $absolute += $sum
            $maxChannel = [Math]::Max($maxChannel, [Math]::Max($red, [Math]::Max($green, $blue)))
            if ($sum -gt 0) {
                $changed++
            }

            if ($null -ne $diffBitmap) {
                if ($sum -eq 0) {
                    $color = [System.Drawing.Color]::White
                } else {
                    $color = [System.Drawing.Color]::FromArgb(
                        255,
                        255,
                        [Math]::Max(0, 255 - ($red * 8)),
                        [Math]::Max(0, 255 - ($green * 8)))
                }
                $diffBitmap.SetPixel($x, $y, $color)
            }
        }
    }

    [pscustomobject]@{
        comparison = $mode
        rectangle = [pscustomobject]@{ left = $left; top = $top; right = $right; bottom = $bottom }
        pixels = $pixels
        changedPixels = $changed
        changedRatio = if ($pixels -eq 0) { 0 } else { [Math]::Round($changed / $pixels, 8) }
        meanAbsoluteRgbDelta = if ($pixels -eq 0) { 0 } else { [Math]::Round($absolute / ($pixels * 3), 6) }
        maxChannelDelta = $maxChannel
    }
}

function Compare-Cards($sourceBitmap, $targetBitmap, $sourceCards, $dx, $dy, $mode) {
    $rows = @()
    foreach ($card in $sourceCards) {
        $rect = Get-IntRect $card
        $rows += Compare-Region $sourceBitmap $targetBitmap $rect.Left $rect.Top $rect.Right $rect.Bottom $dx $dy $mode $null
    }
    $rows
}

$sourceLayout = Get-Content -LiteralPath $sourceLayoutPath -Raw | ConvertFrom-Json
$sourceAppearance = Get-Content -LiteralPath $sourceAppearancePath -Raw | ConvertFrom-Json
$targetLayout = Get-Content -LiteralPath $targetLayoutPath -Raw | ConvertFrom-Json
$targetReport = Get-Content -LiteralPath $targetReportPath -Raw | ConvertFrom-Json
if (-not $targetReport.passed -or $targetReport.binarySha256 -notmatch '^[a-fA-F0-9]{64}$') {
    throw '目标必须是本轮通过验收且已绑定二进制 SHA 的原生报告'
}
$sourceBitmap = [System.Drawing.Bitmap]::FromFile($sourcePath)
$targetBitmap = [System.Drawing.Bitmap]::FromFile($targetPath)

try {
    if ($sourceBitmap.Width -ne 1280 -or $sourceBitmap.Height -ne 820 -or $targetBitmap.Width -ne 1280 -or $targetBitmap.Height -ne 820) {
        throw "Expected both appearance images to be 1280x820."
    }

    $sourceResult = $sourceLayout.data.result
    $sourceCards = @($sourceResult.cards)
    $targetCards = @($targetLayout.cards)
    if ($sourceCards.Count -ne 2 -or $targetCards.Count -ne 2) {
        throw "Expected exactly two appearance cards in each layout."
    }
    if ($sourceResult.viewport.width -ne $targetLayout.viewport.width -or $sourceResult.viewport.height -ne $targetLayout.viewport.height) {
        throw "Viewport dimensions differ."
    }
    if ($sourceResult.deviceScaleFactor -ne $targetLayout.deviceScaleFactor) {
        throw "Device scale factors differ."
    }

    $sourceHeadings = @($sourceResult.headings | ForEach-Object { $_.text })
    $targetHeadings = @($targetLayout.headings | ForEach-Object { $_.text })
    if (($sourceHeadings -join "\u001f") -ne ($targetHeadings -join "\u001f")) {
        throw "Visible heading state differs."
    }
    for ($i = 0; $i -lt $sourceCards.Count; $i++) {
        foreach ($field in @('width', 'height')) {
            $difference = [Math]::Abs(([double]$sourceCards[$i].$field) - ([double]$targetCards[$i].$field))
            if ($difference -gt 0.001) {
                throw "Card $($i + 1) $field differs by $difference."
            }
        }
    }

    $dxExact = [double]$sourceCards[0].x - [double]$targetCards[0].x
    $dyExact = [double]$sourceCards[0].y - [double]$targetCards[0].y
    $dx = [int][Math]::Round($dxExact)
    $dy = [int][Math]::Round($dyExact)
    $wholeRight = $sourceBitmap.Width - 1
    $wholeBottom = $sourceBitmap.Height - 1

    $rawDiff = New-Object System.Drawing.Bitmap($sourceBitmap.Width, $sourceBitmap.Height, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $alignedDiff = New-Object System.Drawing.Bitmap($sourceBitmap.Width, $sourceBitmap.Height, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $rawGraphics = [System.Drawing.Graphics]::FromImage($rawDiff)
    $alignedGraphics = [System.Drawing.Graphics]::FromImage($alignedDiff)
    $rawGraphics.Clear([System.Drawing.Color]::White)
    $alignedGraphics.Clear([System.Drawing.Color]::White)
    $rawGraphics.Dispose()
    $alignedGraphics.Dispose()

    $rawWhole = Compare-Region $sourceBitmap $targetBitmap 0 0 $wholeRight $wholeBottom 0 0 'whole-viewport-raw-same-coordinate' $rawDiff
    $alignedWhole = Compare-Region $sourceBitmap $targetBitmap 0 0 $wholeRight $wholeBottom $dx $dy 'whole-viewport-integer-aligned' $alignedDiff
    $rawDiff.Save((Join-Path $outputDirectory 'appearance-pixel-diff-whole-raw.png'), [System.Drawing.Imaging.ImageFormat]::Png)
    $alignedDiff.Save((Join-Path $outputDirectory 'appearance-pixel-diff-whole-aligned.png'), [System.Drawing.Imaging.ImageFormat]::Png)
    $rawDiff.Dispose()
    $alignedDiff.Dispose()

    $result = [pscustomobject]@{
        comparison = 'source-ZCode-29628c9-vs-current-native-report'
        source = [pscustomobject]@{
            commit = '29628c9acdb81b703bbd4080c207a0e7ce5e276e'
            path = $sourcePath
            viewport = $sourceResult.viewport
            deviceScaleFactor = $sourceResult.deviceScaleFactor
            userAgent = $sourceAppearance.data.result.userAgent
            classes = $sourceResult.classes
        }
        target = [pscustomobject]@{
            path = $targetPath
            viewport = $targetLayout.viewport
            deviceScaleFactor = $targetLayout.deviceScaleFactor
            userAgent = $targetReport.uiEnvironment.userAgent
            classes = $targetLayout.classes
            binarySha256 = $targetReport.binarySha256
        }
        state = [pscustomobject]@{
            sameViewport = $true
            sameDpr = $true
            sameVisibleSection = 'appearance / 外观'
            headingTextsEqual = $true
            cardCountEqual = $true
            exactClassStringEqual = $false
            sourceOnlyClasses = @('window-maximized')
            intentionalDifferences = @(
                'Source screenshot is HeadlessChrome 154; native screenshot is WebView2 Chrome 148.',
                'Source includes the browser-side window-maximized class; native uses its desktop window state.',
                'Branding and product cropping are intentionally outside pixel-equivalence claims.',
                'Native/window chrome and browser rasterization are outside the web content comparison.'
            )
        }
        cardGeometry = [pscustomobject]@{
            source = $sourceCards
            target = $targetCards
            dimensionsEqual = $true
            exactDelta = [pscustomobject]@{ x = $dxExact; y = $dyExact }
            integerAlignment = [pscustomobject]@{ dx = $dx; dy = $dy; residualY = [Math]::Round($dyExact - $dy, 8) }
            alignedRegionDefinition = 'Source coordinates are sampled against target coordinates x-dx,y-dy; full-viewport metrics below are independent of the card regions.'
        }
        wholeViewport = [pscustomobject]@{
            dimensions = '1280x820'
            raw = $rawWhole
            aligned = $alignedWhole
            rawArtifact = 'appearance-pixel-diff-whole-raw.png'
            alignedArtifact = 'appearance-pixel-diff-whole-aligned.png'
        }
        cardRegions = [pscustomobject]@{
            raw = Compare-Cards $sourceBitmap $targetBitmap $sourceCards 0 0 'card-regions-raw-same-coordinate'
            aligned = Compare-Cards $sourceBitmap $targetBitmap $sourceCards $dx $dy 'card-regions-integer-aligned'
            interpretation = 'Supplemental card geometry evidence only; it is not a whole-page equivalence result.'
        }
        interpretation = 'The pixel artifacts compare the complete 1280x820 viewport. They document visual deltas under matched viewport/DPR and do not claim whole-product equivalence.'
    }
    $result | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $outputDirectory 'appearance-pixel-diff.json') -Encoding utf8
    $result | ConvertTo-Json -Depth 20
}
finally {
    $sourceBitmap.Dispose()
    $targetBitmap.Dispose()
}
