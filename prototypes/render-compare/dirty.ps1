# Compare le rendu logiciel complet et par regions sales (fenetre VISIBLE : hors ecran, GDI ne copie
# presque rien et fausserait les chiffres). Chaque execution dure ~5-10 s, sans prise de focus.
# Usage : .\dirty.ps1 [-Runs 3] [-Size 1280x800]
param([int]$Runs = 3, [string]$Size = "")
$ErrorActionPreference = "Stop"
# Garde-fou : une fenetre visible plus grande que 1600x1000 couvre les ecrans de l'utilisateur.
if ($Size -match '^(\d+)x(\d+)$' -and ([int]$Matches[1] -gt 1600 -or [int]$Matches[2] -gt 1000)) {
    throw "Taille $Size refusee : fenetre visible trop grande. Ne pas lancer sans l'accord explicite de l'utilisateur."
}
$exe = Join-Path $PSScriptRoot "target\release\d2d-sw.exe"
function Med($v) { $s = @($v | Sort-Object); $s[[math]::Floor(($s.Count - 1) / 2)] }
foreach ($dirty in "0", "1") {
    $sc = @(); $scp = @(); $lt = @(); $ltp = @()
    for ($i = 0; $i -lt $Runs; $i++) {
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $exe; $psi.RedirectStandardOutput = $true; $psi.UseShellExecute = $false
        $psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
        $psi.EnvironmentVariables["NOFOCUS"] = "1"; $psi.EnvironmentVariables["DIRTY"] = $dirty
        if ($Size) { $psi.EnvironmentVariables["SIZE"] = $Size }
        $p = [System.Diagnostics.Process]::Start($psi)
        if (-not $p.WaitForExit(90000)) { $p.Kill(); Write-Warning "timeout"; continue }
        $o = $p.StandardOutput.ReadToEnd()
        if ($o -match 'd\S+filement[^:]*:\s*moy ([\d.,]+) ms[^\n]*p99 ([\d.,]+)') { $sc += [double]($Matches[1] -replace ',', '.'); $scp += [double]($Matches[2] -replace ',', '.') }
        if ($o -match '-> image pr\S+sent\S+e\s*:\s*moy ([\d.,]+) ms[^\n]*p99 ([\d.,]+)') { $lt += [double]($Matches[1] -replace ',', '.'); $ltp += [double]($Matches[2] -replace ',', '.') }
    }
    "{0,-12} taille={1,-10} defilement {2,6:N2} ms/img (p99 {3,6:N2}) | frappe {4,6:N2} ms (p99 {5,6:N2})" -f `
        $(if ($dirty -eq "1") { "regions" } else { "complet" }), $(if ($Size) { $Size } else { "1280x800" }), (Med $sc), (Med $scp), (Med $lt), (Med $ltp)
}
