param([switch]$UpdateLock)
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$fontDir = Join-Path $root "fonts"
$lockPath = Join-Path $PSScriptRoot "fonts.lock.json"
New-Item -ItemType Directory -Force -Path $fontDir | Out-Null

# google/fonts commit pinned on 2026-11-02.
$commit = "6c1dbe27dcf7d817d4b62cec033ba77fa8e2a5ec"

$files = @(
  @{ Name = "unifont-17.0.05.otf"; Url = "https://unifoundry.com/pub/unifont/unifont-17.0.05/font-builds/unifont-17.0.05.otf" },
  @{ Name = "Jigmo.zip"; Url = "https://kamichikoichi.github.io/jigmo/Jigmo-20250912.zip"; Zip = $true },
  @{ Name = "NotoSans-VF.ttf"; Url = "https://raw.githubusercontent.com/google/fonts/$commit/ofl/notosans/NotoSans%5Bwdth,wght%5D.ttf" },
  @{ Name = "NotoSansSymbols2-Regular.ttf"; Url = "https://raw.githubusercontent.com/google/fonts/$commit/ofl/notosanssymbols2/NotoSansSymbols2-Regular.ttf" },
  @{ Name = "NotoSansMath-Regular.ttf"; Url = "https://raw.githubusercontent.com/google/fonts/$commit/ofl/notosansmath/NotoSansMath-Regular.ttf" },
  @{ Name = "NotoSansJP-VF.ttf"; Url = "https://raw.githubusercontent.com/google/fonts/$commit/ofl/notosansjp/NotoSansJP%5Bwght%5D.ttf" },
  @{ Name = "NotoSerifJP-VF.ttf"; Url = "https://raw.githubusercontent.com/google/fonts/$commit/ofl/notoserifjp/NotoSerifJP%5Bwght%5D.ttf" }
)

function Get-Sha256([string]$Path) {
  (Get-FileHash -Algorithm SHA256 -Path $Path).Hash.ToLowerInvariant()
}

function Download([string]$Url, [string]$Dest) {
  Write-Host "downloading $Url"
  Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Dest
}

$lock = $null
if (Test-Path $lockPath) {
  $lock = Get-Content -Raw $lockPath | ConvertFrom-Json
}

if (-not $UpdateLock -and $null -eq $lock) {
  throw "fonts.lock.json is missing; run: scripts/fetch-fonts.ps1 -UpdateLock"
}

# Download or verify non-zip files.
$final = @{}
foreach ($f in $files | Where-Object { -not $_.Zip }) {
  $dest = Join-Path $fontDir $f.Name
  $expected = if ($null -ne $lock -and $lock.PSObject.Properties.Name -contains $f.Name) {
    $lock.($f.Name).sha256
  } else { $null }

  if (-not $UpdateLock -and (Test-Path $dest) -and $expected -and (Get-Sha256 $dest) -eq $expected) {
    Write-Host "ok $($f.Name)"
    $final[$f.Name] = $expected
    continue
  }

  Download $f.Url $dest
  $actual = Get-Sha256 $dest
  if (-not $UpdateLock -and $expected -and $actual -ne $expected) {
    throw "hash mismatch for $($f.Name): expected $expected, got $actual"
  }
  $final[$f.Name] = $actual
}

# Jigmo ships as a zip of three faces.
$jigmoZip = Join-Path $fontDir "Jigmo.zip"
$jigmoNames = @("Jigmo.ttf", "Jigmo2.ttf", "Jigmo3.ttf")
$needJigmo = $UpdateLock
if (-not $needJigmo) {
  foreach ($name in $jigmoNames) {
    $dest = Join-Path $fontDir $name
    $expected = if ($lock.PSObject.Properties.Name -contains $name) { $lock.$name.sha256 } else { $null }
    if (-not (Test-Path $dest) -or -not $expected -or (Get-Sha256 $dest) -ne $expected) {
      $needJigmo = $true
      break
    }
  }
}

if ($needJigmo) {
  Download "https://kamichikoichi.github.io/jigmo/Jigmo-20250912.zip" $jigmoZip
  $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("glyphmosaic-jigmo-" + [guid]::NewGuid())
  New-Item -ItemType Directory -Force -Path $tmp | Out-Null
  try {
    Expand-Archive -Path $jigmoZip -DestinationPath $tmp -Force
    foreach ($name in $jigmoNames) {
      $found = Get-ChildItem -Path $tmp -Recurse -Filter $name | Select-Object -First 1
      if (-not $found) { throw "Jigmo archive did not contain $name" }
      Copy-Item -Path $found.FullName -Destination (Join-Path $fontDir $name) -Force
    }
  } finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    Remove-Item -Force $jigmoZip -ErrorAction SilentlyContinue
  }
}

foreach ($name in $jigmoNames) {
  $final[$name] = Get-Sha256 (Join-Path $fontDir $name)
}

if ($UpdateLock -or -not (Test-Path $lockPath)) {
  $out = [ordered]@{}
  foreach ($f in $files) {
    if ($f.Zip) { continue }
    $out[$f.Name] = [ordered]@{ url = $f.Url; sha256 = $final[$f.Name] }
  }
  foreach ($name in $jigmoNames) {
    $out[$name] = [ordered]@{
      url = "https://kamichikoichi.github.io/jigmo/Jigmo-20250912.zip"
      sha256 = $final[$name]
    }
  }
  $out | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 $lockPath
  Write-Host "wrote $lockPath"
} else {
  foreach ($name in $final.Keys) {
    $expected = $lock.$name.sha256
    if ($final[$name] -ne $expected) {
      throw "hash mismatch for $name`: expected $expected, got $($final[$name])"
    }
  }
}

$count = (Get-ChildItem $fontDir -File | Measure-Object).Count
Write-Host "fonts ready: $count files in $fontDir"
