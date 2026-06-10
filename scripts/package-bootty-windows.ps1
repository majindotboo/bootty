$ErrorActionPreference = "Stop"

$AppName = "Bootty"
$BinaryName = "bootty.exe"
$PackageName = "bootty-app"
$DistDir = if ($env:BOOTTY_DIST_DIR) { $env:BOOTTY_DIST_DIR } else { "dist" }
$TargetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
$Arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
$BundleName = "$AppName-windows-$Arch"
$BundleRoot = Join-Path $DistDir $BundleName

if (Test-Path $DistDir) {
    Remove-Item -Recurse -Force $DistDir
}
New-Item -ItemType Directory -Force -Path $BundleRoot | Out-Null

cargo build --release -p $PackageName --bin bootty

Copy-Item (Join-Path $TargetRoot "release\$BinaryName") (Join-Path $BundleRoot $BinaryName)
Copy-Item "crates\bootty-app\assets\bootty-mascot.png" (Join-Path $BundleRoot "bootty-mascot.png")
Copy-Item "crates\bootty-app\assets\bootty-mascot.svg" (Join-Path $BundleRoot "bootty-mascot.svg")

$Readme = @"
Bootty
======

Run bootty.exe to start the native Bootty terminal app.
"@
Set-Content -Path (Join-Path $BundleRoot "README.txt") -Value $Readme -NoNewline

$ArchivePath = Join-Path $DistDir "$BundleName.zip"
Compress-Archive -Path $BundleRoot -DestinationPath $ArchivePath -Force
Get-ChildItem -Recurse -File $DistDir | ForEach-Object { $_.FullName }
