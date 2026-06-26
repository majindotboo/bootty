$ErrorActionPreference = "Stop"

$AppName = "Bootty"
$BinaryName = "bootty.exe"
$PackageName = "bootty-app"
$DistDir = if ($env:BOOTTY_DIST_DIR) { $env:BOOTTY_DIST_DIR } else { "dist" }
$TargetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
$Arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
$BundleName = "$AppName-windows-$Arch"
$BundleRoot = Join-Path $DistDir $BundleName
$ReleaseDir = Join-Path $TargetRoot "release"

if (Test-Path $DistDir) {
    Remove-Item -Recurse -Force $DistDir
}
New-Item -ItemType Directory -Force -Path $BundleRoot | Out-Null

cargo build --release -p $PackageName --bin bootty
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE"
}

$BinaryPath = Join-Path $ReleaseDir $BinaryName
if (-not (Test-Path -LiteralPath $BinaryPath)) {
    throw "Expected built binary at $BinaryPath"
}
Copy-Item -LiteralPath $BinaryPath -Destination (Join-Path $BundleRoot $BinaryName)

$RuntimeDlls = @("ghostty-vt.dll")
foreach ($DllName in $RuntimeDlls) {
    $Candidates = @(Get-ChildItem -LiteralPath $ReleaseDir -Recurse -File -Filter $DllName -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTimeUtc -Descending)
    if ($Candidates.Count -eq 0) {
        throw "Expected runtime DLL $DllName under $ReleaseDir"
    }
    Copy-Item -LiteralPath $Candidates[0].FullName -Destination (Join-Path $BundleRoot $DllName)
}

$ArchivePath = Join-Path $DistDir "$BundleName.zip"
Compress-Archive -Path $BundleRoot -DestinationPath $ArchivePath -Force
Get-ChildItem -Recurse -File $DistDir | ForEach-Object { $_.FullName }
