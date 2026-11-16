# Mesures de l'application reelle (binaire release) : demarrage a froid, latence de frappe,
# defilement. Les rapports sont ecrits par l'application dans un fichier (--bench-out) car le
# binaire release n'a pas de console.
#
#   cargo build --release -p paulwrite-app
#   .\scripts\bench-app.ps1 [-Runs 7] [-Typing 300] [-Scroll 600] [-File chemin]
#
# - Demarrage : la fenetre apparait ~100 ms a l'ecran (sans prendre le focus), une fois par execution.
# - Frappe, coloration et defilement : fenetre HORS de la zone visible de l'ecran (rien ne gene l'utilisateur).
#   La copie vers la fenetre y est un peu sous-estimee (rien a afficher) : voir docs/benchmarks.md.
param([int]$Runs = 7, [int]$Typing = 300, [int]$Scroll = 600, [int]$Syntax = 300, [string]$File = "")
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root "target\release\paulwrite.exe"
if (-not (Test-Path $exe)) { throw "absent : $exe (cargo build --release -p paulwrite-app)" }
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "paulwrite-bench-$PID.txt"

function Run-Bench([string[]]$ArgList) {
    Remove-Item $tmp -ErrorAction SilentlyContinue
    $all = @($ArgList) + @("--bench-out", $tmp)
    if ($File) { $all += $File }
    $p = Start-Process -FilePath $exe -ArgumentList $all -PassThru
    if (-not $p.WaitForExit(120000)) { $p.Kill(); throw "delai depasse : $ArgList" }
    if (Test-Path $tmp) { return [System.IO.File]::ReadAllText($tmp, [Text.Encoding]::UTF8) } else { return "" }
}
function Med($v) { $s = @($v | Sort-Object); $s[[math]::Floor(($s.Count - 1) / 2)] }
function Num($text, $pattern) { if ($text -match $pattern) { [double]($Matches[1] -replace ',', '.') } else { [double]::NaN } }

# Demarrage : une execution de chauffe (analyse antivirus du 1er lancement), puis N mesures.
Run-Bench @("--bench-startup") | Out-Null
$vis = @(); $first = @()
for ($i = 0; $i -lt $Runs; $i++) {
    $o = Run-Bench @("--bench-startup")
    $vis += Num $o 'visible ([\d.,]+) ms'
    $first += Num $o '1re image ([\d.,]+) ms'
}
"DEMARRAGE ($Runs runs) : fenetre visible mediane {0:N0} ms ({1:N0}-{2:N0}) | 1re image mediane {3:N0} ms ({4:N0}-{5:N0})" -f `
    (Med $vis), ($vis | Measure-Object -Minimum).Minimum, ($vis | Measure-Object -Maximum).Maximum, `
    (Med $first), ($first | Measure-Object -Minimum).Minimum, ($first | Measure-Object -Maximum).Maximum

$o = Run-Bench @("--bench-typing", "$Typing")
"FRAPPE ($Typing frappes simulees, 1 toutes les 20 ms) :"
($o -split "`n" | Where-Object { $_ -match 'frappe' }) -join "`n"
$o = Run-Bench @("--bench-scroll", "$Scroll")
"DEFILEMENT ($Scroll images, 3 lignes par image) :"
($o -split "`n" | Where-Object { $_ -match 'filement' }) -join "`n"
$o = Run-Bench @("--bench-syntax", "$Syntax")
"COLORATION ($Syntax frappes dans un fichier Rust de ~1 Mo, une toutes les 20 ms) :"
($o -split "`n" | Where-Object { $_ -match 'frappe|coloration' }) -join "`n"
Remove-Item $tmp -ErrorAction SilentlyContinue
