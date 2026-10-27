# CodeGraph installer for Windows: downloads the release binary, verifies
# its checksum and puts it on your user PATH.
#
#   irm https://raw.githubusercontent.com/gzarog/codegraph/main/install.ps1 | iex
#
# Environment:
#   CODEGRAPH_VERSION  release tag to install (default: latest)
#   CODEGRAPH_BIN_DIR  where the binary goes (default: %LOCALAPPDATA%\codegraph\bin)
$ErrorActionPreference = 'Stop'
$Repo = 'gzarog/codegraph'
$BinDir = if ($env:CODEGRAPH_BIN_DIR) { $env:CODEGRAPH_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'codegraph\bin' }

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
  'AMD64' { 'x86_64' }
  'ARM64' { 'aarch64' }
  default { throw "codegraph: unsupported architecture $($env:PROCESSOR_ARCHITECTURE)" }
}
$asset = "codegraph-windows-$arch.zip"

$version = $env:CODEGRAPH_VERSION
if (-not $version) {
  $release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest" -Headers @{ 'User-Agent' = 'codegraph-installer' }
  $version = $release.tag_name
}
if (-not $version.StartsWith('v')) { $version = "v$version" }

$base = "https://github.com/$Repo/releases/download/$version"
$tmp = Join-Path ([IO.Path]::GetTempPath()) "codegraph-$([guid]::NewGuid())"
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
  Write-Host "Installing CodeGraph $version (windows-$arch)..."
  Invoke-WebRequest "$base/$asset" -OutFile (Join-Path $tmp $asset) -UseBasicParsing
  Invoke-WebRequest "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS') -UseBasicParsing
  $expected = (Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { ($_ -split '\s+')[1] -in @($asset, "*$asset") } | ForEach-Object { ($_ -split '\s+')[0] }) | Select-Object -First 1
  $actual = (Get-FileHash (Join-Path $tmp $asset) -Algorithm SHA256).Hash
  if (-not $expected -or $expected -ne $actual.ToLower()) { throw "codegraph: checksum mismatch for $asset; not installing." }
  Expand-Archive (Join-Path $tmp $asset) -DestinationPath $tmp -Force
  New-Item -ItemType Directory -Force $BinDir | Out-Null
  $dest = Join-Path $BinDir 'codegraph.exe'
  if (Test-Path $dest) { Move-Item $dest "$dest.old" -Force }
  Copy-Item (Join-Path $tmp 'codegraph.exe') $dest -Force
  Remove-Item "$dest.old" -Force -ErrorAction SilentlyContinue
  Write-Host "Installed $dest"
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (-not ($userPath -split ';' | Where-Object { $_ -eq $BinDir })) {
    [Environment]::SetEnvironmentVariable('Path', "$BinDir;$userPath", 'User')
    Write-Host "Added $BinDir to your user PATH (open a new terminal)."
  }
  Write-Host "Next: run 'codegraph install' to connect your coding agents."
} finally {
  Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
