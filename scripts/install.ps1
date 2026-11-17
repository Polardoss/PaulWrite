# Installs PaulWrite for the current user (no administrator rights needed).
#
#   .\install.ps1                          # installs and adds the Start Menu entry and "Open with PaulWrite"
#   .\install.ps1 -Associate .txt,.md,.rs  # also lists PaulWrite in "Open with" for these extensions
#   .\install.ps1 -AddToPath               # also lets you type `paulwrite file` in a terminal
#   .\install.ps1 -DryRun                  # shows what would be done, changes nothing
#
# Everything is written under your user account: %LOCALAPPDATA%\Programs\PaulWrite, the Start Menu
# of your profile and HKEY_CURRENT_USER. Windows does not let a program make itself the default
# editor of a file type: after -Associate, use "Open with > Choose another app > Always".
# Remove everything with uninstall.ps1 (also listed in "Installed apps").
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA "Programs\PaulWrite"),
    [string[]]$Associate = @(),
    [switch]$AddToPath,
    [switch]$NoShortcut,
    [switch]$NoContextMenu,
    [switch]$DryRun,
    # Registry locations (changed only by the tests of this script).
    [string]$ClassesKey = "HKCU:\Software\Classes",
    [string]$UninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\PaulWrite"
)
$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$source = Join-Path $here "paulwrite.exe"
if (-not (Test-Path $source)) { throw "paulwrite.exe not found next to install.ps1 ($here)" }
$version = (Get-Item $source).VersionInfo.ProductVersion
$exe = Join-Path $InstallDir "paulwrite.exe"

function Step($text) { Write-Host ("{0}{1}" -f $(if ($DryRun) { "[dry run] " } else { "" }), $text) }
function Set-Key($path, $name, $value) {
    if ($DryRun) { return }
    # -LiteralPath: the "*" key (all files) must not be read as a wildcard
    if (-not (Test-Path -LiteralPath $path)) { New-Item -Path $path -Force | Out-Null }
    if ($name -eq "") { Set-Item -LiteralPath $path -Value $value } else { Set-ItemProperty -LiteralPath $path -Name $name -Value $value }
}

# 1. Files
Step "Copying files to $InstallDir"
if (-not $DryRun) {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    foreach ($f in "paulwrite.exe", "uninstall.ps1", "LICENSE", "README.md") {
        $p = Join-Path $here $f
        if (Test-Path $p) {
            try { Copy-Item $p $InstallDir -Force }
            catch { throw "Cannot replace $f in ${InstallDir}: close PaulWrite and run the script again. ($_)" }
        }
    }
}

# 2. Start Menu shortcut
if (-not $NoShortcut) {
    $lnk = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\PaulWrite.lnk"
    Step "Start Menu shortcut: $lnk"
    if (-not $DryRun) {
        $sh = New-Object -ComObject WScript.Shell
        $s = $sh.CreateShortcut($lnk)
        $s.TargetPath = $exe
        $s.WorkingDirectory = $InstallDir
        $s.Description = "PaulWrite - fast text editor"
        $s.IconLocation = "$exe,0"
        $s.Save()
    }
}

# 3. "Open with PaulWrite" in the context menu of files, folders and folder backgrounds
if (-not $NoContextMenu) {
    Step "Context menu entries (files, folders)"
    foreach ($t in @(@("*\shell\PaulWrite", "%1"), @("Directory\shell\PaulWrite", "%1"), @("Directory\Background\shell\PaulWrite", "%V"))) {
        $key = Join-Path $ClassesKey $t[0]
        Set-Key $key "" "Open with PaulWrite"
        Set-Key $key "Icon" "`"$exe`""
        Set-Key (Join-Path $key "command") "" "`"$exe`" `"$($t[1])`""
    }
}

# 4. Optional: list PaulWrite in "Open with" for some extensions
if ($Associate.Count -gt 0) {
    Step "Open-with entries for: $($Associate -join ', ')"
    $prog = Join-Path $ClassesKey "PaulWrite.File"
    Set-Key $prog "" "PaulWrite document"
    Set-Key (Join-Path $prog "DefaultIcon") "" "`"$exe`",0"
    Set-Key (Join-Path $prog "shell\open\command") "" "`"$exe`" `"%1`""
    foreach ($ext in $Associate) {
        if (-not $ext.StartsWith(".")) { $ext = ".$ext" }
        Set-Key (Join-Path $ClassesKey "$ext\OpenWithProgids") "PaulWrite.File" ""
    }
}

# 5. Optional: user PATH
if ($AddToPath) {
    Step "Adding $InstallDir to the user PATH"
    if (-not $DryRun) {
        $current = [Environment]::GetEnvironmentVariable("Path", "User")
        if (($current -split ";") -notcontains $InstallDir) {
            [Environment]::SetEnvironmentVariable("Path", ($current.TrimEnd(";") + ";" + $InstallDir), "User")
        }
    }
}

# 6. "Installed apps" entry (uninstall)
Step "Registering in Installed apps (version $version)"
$size = if ($DryRun) { 0 } else { [int]((Get-ChildItem $InstallDir -Recurse | Measure-Object Length -Sum).Sum / 1KB) }
Set-Key $UninstallKey "DisplayName" "PaulWrite"
Set-Key $UninstallKey "DisplayVersion" $version
Set-Key $UninstallKey "Publisher" "PaulWrite contributors"
Set-Key $UninstallKey "InstallLocation" $InstallDir
Set-Key $UninstallKey "DisplayIcon" $exe
Set-Key $UninstallKey "EstimatedSize" $size
Set-Key $UninstallKey "NoModify" 1
Set-Key $UninstallKey "NoRepair" 1
$un = Join-Path $InstallDir "uninstall.ps1"
$cmd = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$un`""
if ($ClassesKey -ne "HKCU:\Software\Classes") { $cmd += " -ClassesKey `"$ClassesKey`" -UninstallKey `"$UninstallKey`"" }
Set-Key $UninstallKey "UninstallString" $cmd

Step "Done. Run: $exe"
