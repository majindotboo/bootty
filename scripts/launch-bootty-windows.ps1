$ErrorActionPreference = "Stop"

$BinaryName = "bootty.exe"
$TargetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
$ProfileName = "dynamic-release"

& pwsh ./scripts/build-bootty-windows.ps1
if ($LASTEXITCODE -ne 0) {
    throw "Windows build failed with exit code $LASTEXITCODE"
}

$ProfileDir = Join-Path $TargetRoot $ProfileName
$BinaryPath = Join-Path $ProfileDir $BinaryName
if (-not (Test-Path -LiteralPath $BinaryPath)) {
    throw "Built binary not found at $BinaryPath"
}

$PathDirs = @($ProfileDir, (Join-Path $ProfileDir "deps"))
$RustLibDir = (rustc --print target-libdir) | Select-Object -First 1
if ($RustLibDir) {
    $PathDirs += $RustLibDir
}
$GhosttyDll = Get-ChildItem -LiteralPath $ProfileDir -Recurse -File -Filter "ghostty-vt.dll" -ErrorAction SilentlyContinue |
    Select-Object -First 1
if ($GhosttyDll) {
    $PathDirs += $GhosttyDll.DirectoryName
}
$env:PATH = (($PathDirs | Where-Object { $_ -and (Test-Path -LiteralPath $_) }) -join ";") + ";$env:PATH"

& $BinaryPath @args
exit $LASTEXITCODE
