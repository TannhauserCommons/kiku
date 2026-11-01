# Publishes the model files the catalog serves from Kiku's releases (URLs under "{release}/", now only
# Parakeet Unified fp16) as assets of this repository's release the catalog names (models-v1), if they
# aren't there yet. Run by CI on each release, with GITHUB_REPOSITORY and GH_TOKEN.
# Each is made from its model's fp32 variant with scripts\make-fp16.py, whose pinned versions give the
# catalog's exact bytes; scripts\catalog.py stage checks every sha256 before anything is uploaded.
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$Project = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Models = Join-Path $Project 'target\models'
$Staged = Join-Path $Project 'target\models-staged'
$Repo = $env:GITHUB_REPOSITORY
$Release = '{release}/'

function Save-Files($dir, $files) {
    New-Item -ItemType Directory -Force (Join-Path $Models $dir) | Out-Null
    foreach ($file in $files) {
        $target = Join-Path $Models (Join-Path $dir $file.path)
        Invoke-WebRequest $file.url -OutFile $target
        if ((Get-FileHash $target -Algorithm SHA256).Hash -ne $file.sha256) { throw "$($file.path) differs from the catalog" }
    }
}

$catalog = Get-Content (Join-Path $Project 'crates\core\catalog.json') -Raw | ConvertFrom-Json
$tag = Split-Path $catalog.release -Leaf
$PSNativeCommandUseErrorActionPreference = $false
$published = @(gh release view $tag --repo $Repo --json assets --jq '.assets[] | "\(.name) \(.size)"' 2>$null)
$exists = $LASTEXITCODE -eq 0
$PSNativeCommandUseErrorActionPreference = $true

$missing = @()
foreach ($model in $catalog.models) {
    foreach ($variant in $model.variants) {
        $served = @($variant.files | Where-Object { $_.url.StartsWith($Release) -and "$($_.url.Substring($Release.Length)) $($_.size)" -notin $published })
        if (-not $served) { continue }
        $source = $model.variants | Where-Object precision -eq 'fp32' | Select-Object -First 1
        "Making $($model.id) $($variant.precision) from $($source.dir)"
        Save-Files $source.dir $source.files
        uv run (Join-Path $PSScriptRoot 'make-fp16.py') (Join-Path $Models $source.dir) (Join-Path $Models $variant.dir)
        $missing += $served
    }
}
if (-not $missing) { return "Every model file is published already" }

python (Join-Path $PSScriptRoot 'catalog.py') stage $Staged $Models
if (-not $exists) {
    gh release create $tag --repo $Repo --title 'Model files' --notes 'Model files Kiku downloads when you pick them.' --latest=false
}
$files = $missing | ForEach-Object { Join-Path $Staged $_.url.Substring($Release.Length) }
gh release upload $tag --repo $Repo --clobber @files
"Published $($files.Count) model files in $tag"
