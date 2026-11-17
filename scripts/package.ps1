# Builds the release version and packages it: dist\PaulWrite-<version>-windows-x64.zip
# (paulwrite.exe, the install and uninstall scripts, LICENSE, README) and its SHA-256 checksum.
#
#   .\scripts\package.ps1              # build + package
#   .\scripts\package.ps1 -SkipBuild   # package the existing target\release\paulwrite.exe
param([switch]$SkipBuild)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path" }

$manifest = Get-Content (Join-Path $root "Cargo.toml") -Raw
if ($manifest -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw "version not found in Cargo.toml" }
$version = $Matches[1]
$name = "PaulWrite-$version-windows-x64"

if (-not $SkipBuild) {
    cargo build --release -p paulwrite-app
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
}
$exe = Join-Path $root "target\release\paulwrite.exe"
if (-not (Test-Path $exe)) { throw "missing: $exe" }
$built = (Get-Item $exe).VersionInfo.ProductVersion
if ($built -ne $version) { throw "paulwrite.exe is version $built but Cargo.toml says ${version}: rebuild (omit -SkipBuild)" }

$dist = Join-Path $root "dist"
$stage = Join-Path $dist $name
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item $exe $stage
foreach ($f in "scripts\install.ps1", "scripts\uninstall.ps1", "LICENSE", "README.md") { Copy-Item (Join-Path $root $f) $stage }

$zip = Join-Path $dist "$name.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash  $name.zip" | Set-Content -Path "$zip.sha256" -Encoding ascii
"{0}  ({1:N1} MB, exe {2:N1} MB)" -f $zip, ((Get-Item $zip).Length / 1MB), ((Get-Item $exe).Length / 1MB)
"SHA-256 $hash"
