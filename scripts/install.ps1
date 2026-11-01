# Installs the release build to %LOCALAPPDATA%\Programs\kiku and adds a Start menu shortcut.
# Run it yourself (not from an agent sandbox): files there must not inherit a Low integrity label,
# or Windows blocks the tray icon and saving to %APPDATA%.
#   -Small     installs what a new user gets: the app and its CPU runtime with the speech detector,
#              but no models and no GPU pack, which the app downloads (about 45 MB instead of 11 GB).
#   -Target    another folder, e.g. to try -Small beside your own install.
param([switch]$Small, [string]$Target = (Join-Path $env:LOCALAPPDATA 'Programs\kiku'))
$ErrorActionPreference = 'Stop'
$Default = $Target -eq (Join-Path $env:LOCALAPPDATA 'Programs\kiku')

$Project = Join-Path $PSScriptRoot '..'
$Release = Join-Path $Project 'target\release'

# Copies only files that are new or differ in size or time: the DLLs and models are ~11 GB.
# Without /MIR, robocopy deletes nothing from the target.
function Sync($From, $To, [string[]]$Files, [switch]$Recurse) {
    $PSNativeCommandUseErrorActionPreference = $false
    $options = '/R:2', '/W:1', '/NJH', '/NJS', '/NDL', '/NP'
    if ($Recurse) { $options += '/E' }
    robocopy $From $To @Files @options
    # Exit codes below 8 mean success: 0 nothing to copy, 1 files copied, and so on.
    if ($LASTEXITCODE -ge 8) { throw "Copying $From to $To failed: robocopy exit code $LASTEXITCODE" }
}

if (-not (Test-Path (Join-Path $Release 'kiku.exe'))) { throw "Build first: cargo build --release" }
# Only the installed copy: development builds and benchmarks elsewhere keep running.
# Waits for them to exit, or their files are still in use when the copy starts.
Get-Process kiku, kiku-engine -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Target\*" } | Stop-Process -PassThru | Wait-Process

New-Item -ItemType Directory -Force $Target | Out-Null
# An assistant using it keeps the MCP server running. A running exe can't be overwritten but can be renamed;
# old copies are removed once nothing runs them.
Get-ChildItem $Target -Filter 'kiku-mcp.*.old' | Remove-Item -ErrorAction SilentlyContinue
$mcp = Join-Path $Target 'kiku-mcp.exe'
if (Test-Path $mcp) { Move-Item $mcp (Join-Path $Target "kiku-mcp.$(Get-Date -Format yyyyMMddHHmmss).old") }
if ($Small) {
    # The GPU pack's DLLs are the ones the catalog lists; TensorRT is never used.
    $catalog = Get-Content (Join-Path $Project 'crates\core\catalog.json') -Raw | ConvertFrom-Json
    $gpuPack = ($catalog.components | Where-Object id -eq 'gpu-pack').files.path + 'onnxruntime_providers_tensorrt.dll'
    $dlls = Get-ChildItem $Release -Filter '*.dll' | Where-Object Name -notin $gpuPack | ForEach-Object Name
    Sync $Release $Target (@('kiku.exe', 'kiku-mcp.exe', 'kiku-engine.exe') + $dlls)
    Sync (Join-Path $Project 'models\sherpa') (Join-Path $Target 'models\sherpa') 'silero_vad.onnx'
} else {
    Sync $Release $Target 'kiku.exe', 'kiku-mcp.exe', 'kiku-engine.exe', '*.dll'
    Sync (Join-Path $Project 'models') (Join-Path $Target 'models') -Recurse
}

# A trial install elsewhere leaves the Start menu pointing at the usual one.
if ($Default) {
    $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path ([Environment]::GetFolderPath('Programs')) 'Kiku.lnk'))
    $shortcut.TargetPath = Join-Path $Target 'kiku.exe'
    $shortcut.WorkingDirectory = $Target
    $shortcut.Save()
}

$label = icacls (Join-Path $Target 'kiku.exe') | Select-String 'Mandatory Label'
if ($label) { Write-Warning "Installed exe still has a label: $label" } else { "Installed to $Target (normal integrity). Start it from the Start menu: Kiku." }
