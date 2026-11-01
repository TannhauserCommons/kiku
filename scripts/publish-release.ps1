# Publishes a release CI built as release v<version> of this repository (GITHUB_REPOSITORY, with GH_TOKEN):
# first a draft holding the installer (also as Kiku-setup.exe, the stable download link) and latest.json,
# made the latest release only once every file is up, so latest/download/latest.json never names an
# installer that isn't there.
param([Parameter(Mandatory)][string]$Version, [string]$Notes = '')
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$Dist = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')).Path 'target\dist'
$Repo = $env:GITHUB_REPOSITORY

$setup = Join-Path $Dist "Kiku-$Version-setup.exe"
$stable = Join-Path $Dist 'Kiku-setup.exe'
Copy-Item $setup $stable
$tag = "v$Version"
gh release create $tag --repo $Repo --draft --verify-tag --title "Kiku $Version" --notes $Notes $setup $stable (Join-Path $Dist 'latest.json')
gh release edit $tag --repo $Repo --draft=false --latest | Out-Null
"Published https://github.com/$Repo/releases/latest/download/Kiku-setup.exe"
