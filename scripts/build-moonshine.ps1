# Builds the Moonshine speech-to-text runtime from source into third_party/.
# Needs git, CMake and Visual Studio 2022 (C++ workload). Re-running is safe.
$ErrorActionPreference = 'Stop'

$Tag    = 'v0.1.5'
$Commit = '234f60faa0eb388b01cdf7e60aca232af37aefda'
$Root   = Join-Path $PSScriptRoot '..\third_party'
$Src    = Join-Path $Root 'moonshine-src'
$Build  = Join-Path $Root 'moonshine-build'

if (-not (Test-Path $Src)) {
    git clone --depth 1 --branch $Tag https://github.com/moonshine-ai/moonshine.git $Src
}
$head = git -C $Src rev-parse HEAD
if ($head -ne $Commit) { throw "moonshine-src is at $head, expected $Commit ($Tag)" }

# Our fixes to Moonshine, each applied once.
foreach ($patch in Get-ChildItem (Join-Path $PSScriptRoot '..\patches\moonshine-*.patch')) {
    # The check prints to stderr when the patch isn't applied yet, which 'Stop' would treat as fatal.
    $ErrorActionPreference = 'Continue'
    git -C $Src apply --reverse --check $patch.FullName 2>&1 | Out-Null
    $applied = $LASTEXITCODE -eq 0
    $ErrorActionPreference = 'Stop'
    if (-not $applied) { git -C $Src apply $patch.FullName }
}

# Any Visual Studio generator puts each library in a Release folder, as build.rs expects. Built for this
# PC's architecture, x64 or ARM64, like the runtime fetch-runtime.ps1 fetched.
$platform = @{ X64 = 'x64'; Arm64 = 'ARM64' }["$([Runtime.InteropServices.RuntimeInformation]::OSArchitecture)"]
cmake -S (Join-Path $Src 'core') -B $Build -A $platform
# Moonshine's messages carry __FILE__; trimming the source folder keeps local paths out of release builds.
# The source folder is the one the build was set up with, read from CMake's cache.
$configured = (Select-String -Path (Join-Path $Build 'CMakeCache.txt') -Pattern '^CMAKE_HOME_DIRECTORY:INTERNAL=(.*)$').Matches[0].Groups[1].Value
$env:CL = "/d1trimfile:$((Split-Path $configured) -replace '/', '\')\"
cmake --build $Build --config Release --target moonshine -- /m /v:minimal
