$ErrorActionPreference = "Stop"

$AppName = "Bootty"
$BinaryName = "bootty.exe"
$DistDir = if ($env:BOOTTY_DIST_DIR) { $env:BOOTTY_DIST_DIR } else { "dist" }
$Arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
$BundleName = "$AppName-windows-$Arch"
$BundleRoot = Join-Path $DistDir $BundleName
$InstallDir = if ($env:BOOTTY_INSTALL_DIR) {
    $env:BOOTTY_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA "Programs\Bootty"
}

& pwsh ./scripts/package-bootty-windows.ps1
if ($LASTEXITCODE -ne 0) {
    throw "Windows package failed with exit code $LASTEXITCODE"
}

if (-not (Test-Path -LiteralPath $BundleRoot)) {
    throw "Packaged app not found at $BundleRoot"
}

if (Test-Path -LiteralPath $InstallDir) {
    Remove-Item -LiteralPath $InstallDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item -Path (Join-Path $BundleRoot "*") -Destination $InstallDir -Recurse -Force

$StartMenuDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
if (Test-Path -LiteralPath $StartMenuDir) {
    $ShortcutPath = Join-Path $StartMenuDir "$AppName.lnk"
    $Shell = New-Object -ComObject WScript.Shell
    $Shortcut = $Shell.CreateShortcut($ShortcutPath)
    $Shortcut.TargetPath = Join-Path $InstallDir $BinaryName
    $Shortcut.WorkingDirectory = if ($env:USERPROFILE) { $env:USERPROFILE } else { $InstallDir }
    $Shortcut.IconLocation = Join-Path $InstallDir $BinaryName
    $Shortcut.Save()
}

Write-Output "Installed $(Join-Path $InstallDir $BinaryName)"
