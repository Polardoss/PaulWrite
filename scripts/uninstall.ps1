# Removes what install.ps1 added: files, Start Menu shortcut, context menu entries, "Open with"
# entries, PATH entry and the "Installed apps" record. Your settings (%APPDATA%\PaulWrite) are kept.
#
#   .\uninstall.ps1            # removes everything
#   .\uninstall.ps1 -DryRun    # shows what would be removed
param(
    [string]$InstallDir = $PSScriptRoot,
    [switch]$DryRun,
    # Registry locations (changed only by the tests of this script).
    [string]$ClassesKey = "HKCU:\Software\Classes",
    [string]$SoftwareKey = "HKCU:\Software",
    [string]$UninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\PaulWrite"
)
$ErrorActionPreference = "Stop"

function Step($text) { Write-Host ("{0}{1}" -f $(if ($DryRun) { "[dry run] " } else { "" }), $text) }
function Remove-Key($path) {
    # -LiteralPath : la cle "*" (tous les fichiers) ne doit pas etre prise pour un motif
    if (Test-Path -LiteralPath $path) { Step "Removing $path"; if (-not $DryRun) { Remove-Item -LiteralPath $path -Recurse -Force } }
}

if (Get-Process paulwrite -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $InstallDir "paulwrite.exe") }) {
    throw "PaulWrite is running: close it first."
}

$lnk = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\PaulWrite.lnk"
if (Test-Path $lnk) { Step "Removing $lnk"; if (-not $DryRun) { Remove-Item $lnk -Force } }

foreach ($k in "*\shell\PaulWrite", "Directory\shell\PaulWrite", "Directory\Background\shell\PaulWrite", "PaulWrite.File", "Applications\paulwrite.exe") {
    Remove-Key (Join-Path $ClassesKey $k)
}
# registration in Settings > Default apps
Remove-Key (Join-Path $SoftwareKey "PaulWrite")
$reg = Join-Path $SoftwareKey "RegisteredApplications"
if ((Test-Path -LiteralPath $reg) -and ($null -ne (Get-ItemProperty -LiteralPath $reg -Name "PaulWrite" -ErrorAction SilentlyContinue))) {
    Step "Removing the PaulWrite entry of RegisteredApplications"
    if (-not $DryRun) { Remove-ItemProperty -LiteralPath $reg -Name "PaulWrite" -Force }
}
# "Open with" entries added by -Associate
if (Test-Path $ClassesKey) {
    Get-ChildItem $ClassesKey -ErrorAction SilentlyContinue |
        Where-Object { $_.PSChildName -like ".*" } |
        ForEach-Object {
            $ow = Join-Path $_.PSPath "OpenWithProgids"
            if ((Test-Path -LiteralPath $ow) -and ($null -ne (Get-ItemProperty -LiteralPath $ow -Name "PaulWrite.File" -ErrorAction SilentlyContinue))) {
                Step "Removing the open-with entry of $($_.PSChildName)"
                if (-not $DryRun) { Remove-ItemProperty -LiteralPath $ow -Name "PaulWrite.File" -Force }
            }
        }
}

$path = [Environment]::GetEnvironmentVariable("Path", "User")
if ($path -and (($path -split ";") -contains $InstallDir)) {
    Step "Removing $InstallDir from the user PATH"
    if (-not $DryRun) {
        [Environment]::SetEnvironmentVariable("Path", ((($path -split ";") | Where-Object { $_ -ne $InstallDir -and $_ -ne "" }) -join ";"), "User")
    }
}

Remove-Key $UninstallKey

if (Test-Path $InstallDir) {
    Step "Removing the files of $InstallDir"
    if (-not $DryRun) {
        # this script lives in the folder: delete it from a separate process once it has exited
        $quoted = $InstallDir.Replace('"', '')
        Start-Process cmd.exe -WindowStyle Hidden -ArgumentList "/c timeout /t 2 /nobreak >nul & rmdir /s /q `"$quoted`""
    }
}
Step "Done."
