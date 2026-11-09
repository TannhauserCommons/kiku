# Publishes a release CI built as release v<version> of this repository (GITHUB_REPOSITORY, with GH_TOKEN):
# first a draft holding each architecture's installer, also under a stable name for download links
# (Kiku-setup.exe for x64, Kiku-arm64-setup.exe), and its signed manifest, made the latest release only once
# every file is up, so a manifest never names an installer that isn't there. The Linux model host's tarball goes
# up too (also as kiku-engine-linux-x64.tar.gz). The notes say which file is which.
param([Parameter(Mandatory)][string]$Version, [string]$Notes = '')
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$Dist = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')).Path 'target\dist'
$Repo = $env:GITHUB_REPOSITORY
$Tag = "v$Version"
$Builds = [ordered]@{
    x64   = @{ Stable = 'Kiku-setup.exe'; For = 'Windows 10 and 11 on x64, most PCs' }
    arm64 = @{ Stable = 'Kiku-arm64-setup.exe'; For = 'Windows 11 on ARM, such as Snapdragon laptops' }
}

$files = @(Get-ChildItem $Dist -Filter 'latest*.json' | ForEach-Object FullName)
$downloads = foreach ($arch in $Builds.Keys) {
    $setup = Join-Path $Dist "Kiku-$Version-$arch-setup.exe"
    $stable = Join-Path $Dist $Builds[$arch].Stable
    Copy-Item $setup $stable
    $files += $setup, $stable
    "- $($Builds[$arch].For): [$(Split-Path $setup -Leaf)](https://github.com/$Repo/releases/download/$Tag/$(Split-Path $setup -Leaf))"
}
$tarball = Get-ChildItem $Dist -Recurse -Filter "kiku-engine-$Version-linux-x64.tar.gz" | Select-Object -First 1
if (-not $tarball) { throw "No Linux tarball for $Version in $Dist" }
$stable = Join-Path $Dist 'kiku-engine-linux-x64.tar.gz'
Copy-Item $tarball.FullName $stable
$files += $tarball.FullName, $stable
$image = "ghcr.io/$($Repo.Split('/')[0].ToLower())/kiku-engine"
$downloads = @($downloads) + "- Linux model host (x64), to share models from a Linux PC or server (Ubuntu 22.04 or Debian 12 and newer; HOSTING.md inside says how): [$($tarball.Name)](https://github.com/$Repo/releases/download/$Tag/$($tarball.Name)), or the Docker image ``$image``"
$body = "$Notes`n`n### Downloads`n`n$($downloads -join "`n")".Trim()
gh release create $Tag --repo $Repo --draft --verify-tag --title "Kiku $Version" --notes $body @files
gh release edit $Tag --repo $Repo --draft=false --latest | Out-Null
"Published https://github.com/$Repo/releases/tag/$Tag"
