# Lance chaque variante du prototype plusieurs fois et résume les mesures.
# Usage (depuis prototypes/render-compare) :
#   cargo build --release            # compile les trois binaires
#   .\run.ps1 -Runs 5 [-Vsync]
param([int]$Runs = 5, [switch]$Vsync)

$ErrorActionPreference = "Stop"
$dir = Join-Path $PSScriptRoot "target\release"
$variants = @(
    @{ Name = "Direct2D GPU (D3D11 flip)"; Exe = "d2d.exe";    Env = @{} },
    @{ Name = "Direct2D logiciel (sans GPU)"; Exe = "d2d-sw.exe"; Env = @{} },
    @{ Name = "wgpu (backend par defaut)"; Exe = "wgpu.exe";   Env = @{} },
    @{ Name = "wgpu DX12";                 Exe = "wgpu.exe";   Env = @{ WGPU_BACKEND = "dx12" } },
    @{ Name = "wgpu Vulkan";               Exe = "wgpu.exe";   Env = @{ WGPU_BACKEND = "vulkan" } }
)

function Get-Num($text, $pattern) {
    if ($text -match $pattern) { return [double]($Matches[1] -replace ',', '.') } else { return [double]::NaN }
}
function Median($v) { $s = @($v | Sort-Object); if ($s.Count -eq 0) { return [double]::NaN }; $s[[math]::Floor(($s.Count - 1) / 2)] }

$rows = foreach ($v in $variants) {
    $exe = Join-Path $dir $v.Exe
    if (-not (Test-Path $exe)) { Write-Warning "absent : $exe"; continue }
    $start = @(); $scroll = @(); $scrollP99 = @(); $lat = @(); $latP99 = @(); $ws = @()
    for ($i = 0; $i -lt $Runs; $i++) {
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $exe; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
        $psi.UseShellExecute = $false; $psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
        $psi.EnvironmentVariables["VSYNC"] = $(if ($Vsync) { "1" } else { "0" })
        $psi.EnvironmentVariables["QUIET"] = "1"   # fenetre hors ecran, sans prise de focus
        foreach ($k in $v.Env.Keys) { $psi.EnvironmentVariables[$k] = $v.Env[$k] }
        $p = [System.Diagnostics.Process]::Start($psi)
        $peak = 0
        while (-not $p.HasExited) { try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}; Start-Sleep -Milliseconds 20 }
        $out = $p.StandardOutput.ReadToEnd()
        $start += Get-Num $out 'D\S+MARRAGE \S+ FROID[^:]*:\s*([\d.,]+) ms'
        $scroll += Get-Num $out 'd\S+filement[^:]*:\s*moy ([\d.,]+) ms'
        $scrollP99 += Get-Num $out 'd\S+filement[^:]*:.*?p99 ([\d.,]+)'
        $lat += Get-Num $out '-> image pr\S+sent\S+e\s*:\s*moy ([\d.,]+) ms'
        $latP99 += Get-Num $out '-> image pr\S+sent\S+e\s*:.*?p99 ([\d.,]+)'
        $ws += $peak / 1MB
    }
    [pscustomobject]@{
        Variante      = $v.Name
        'Demarrage ms (mediane)' = [math]::Round((Median $start), 0)
        'min-max'     = ("{0:N0}-{1:N0}" -f ($start | Measure-Object -Minimum).Minimum, ($start | Measure-Object -Maximum).Maximum)
        'Defilement ms/img' = [math]::Round((Median $scroll), 2)
        'p99'         = [math]::Round((Median $scrollP99), 2)
        'Latence frappe ms' = [math]::Round((Median $lat), 2)
        'p99 '        = [math]::Round((Median $latP99), 2)
        'Pic RAM Mo'  = [math]::Round((Median $ws), 0)
    }
}
"vsync = $([bool]$Vsync), $Runs executions par variante (fichiers en cache système)"
$rows | Format-Table -AutoSize | Out-String -Width 220
