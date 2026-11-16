# Mesure du demarrage seul (la fenetre reste a l'ecran ~200 ms puis se ferme, sans prendre le focus).
# Usage : .\startup.ps1 [-Runs 7]   (HIDDEN=1 : fenetre masquee, 1re image, puis affichage)
param([int]$Runs = 7)
$ErrorActionPreference = "Stop"
$dir = Join-Path $PSScriptRoot "target\release"
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$env:NOFOCUS = "1"; $env:EXIT_AFTER_FIRST = "1"
function Med($v) { $s = @($v | Sort-Object); $s[[math]::Floor(($s.Count - 1) / 2)] }
foreach ($bin in "d2d-sw", "d2d") {
    foreach ($h in "0", "1") {
        $env:HIDDEN = $h
        & "$dir\$bin.exe" | Out-Null   # chauffe (antivirus au 1er lancement)
        $vis = @(); $first = @()
        for ($i = 0; $i -lt $Runs; $i++) {
            $o = (& "$dir\$bin.exe") -join "`n"
            if ($o -match 'VISIBLE[^:]*:\s*([\d.,]+) ms') { $vis += [double]($Matches[1] -replace ',', '.') }
            if ($o -match 'FROID[^:]*:\s*([\d.,]+) ms') { $first += [double]($Matches[1] -replace ',', '.') }
        }
        "{0,-7} hidden={1}  fenetre visible+image : mediane {2,5:N0} ms ({3:N0}-{4:N0})  | 1re image : mediane {5,5:N0} ms" -f `
            $bin, $h, (Med $vis), ($vis | Measure-Object -Minimum).Minimum, ($vis | Measure-Object -Maximum).Maximum, (Med $first)
    }
}
Remove-Item Env:NOFOCUS, Env:EXIT_AFTER_FIRST, Env:HIDDEN
