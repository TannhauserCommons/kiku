# Builds what a release ships into target\dist\Kiku: the app, its MCP server and GPU helper, the CPU
# runtime and the speech detector (what scripts\install.ps1 -Small installs), with no local paths inside.
# The installer (installer\kiku.iss) packs that folder; scripts\privacy-gate.py checks it first.
#   -Test      runs the tests first (not doc tests), built the same way so the release build reuses them.
# KIKU_RELEASES (where releases are published) and KIKU_UPDATE_KEY (the key updates are signed
# with) are compiled in when set; without them the build never checks for updates.
param([switch]$Test)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$Project = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
# Builds are native: x64 or ARM64, as this PC is.
$Arch = "$([Runtime.InteropServices.RuntimeInformation]::OSArchitecture)".ToLower()
$CargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$Out = Join-Path $Project 'target\dist\Kiku'

# Panic locations name no folder of this machine, and the PDB path written into each exe is its bare name.
# Encoded flags, since paths may hold spaces.
$flags = "--remap-path-prefix=$Project=kiku", "--remap-path-prefix=$CargoHome=cargo", '-Clink-arg=/PDBALTPATH:%_PDB%'
$env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f
$env:CARGO_TARGET_DIR = Join-Path $Project 'target\dist'
# C and C++ built by crates (SQLite, ring) keep __FILE__ relative to where they are compiled. On ARM64 ring
# needs clang, so all C is built with it, which trims paths its own way.
$env:CFLAGS = $env:CXXFLAGS = "/d1trimfile:$Project\ /d1trimfile:$CargoHome\"
if ($Arch -eq 'arm64') {
    $env:CC = 'clang'
    $env:CFLAGS = "-ffile-prefix-map=$Project\= -ffile-prefix-map=$CargoHome\="
}

# Copies a catalog component into a models folder from models\ here, or downloads it, checking each file.
function Save-Component($catalog, $id, $models) {
    $component = $catalog.components | Where-Object id -eq $id
    foreach ($file in $component.files) {
        $target = Join-Path $models (Join-Path $component.dir $file.path)
        New-Item -ItemType Directory -Force (Split-Path $target) | Out-Null
        $local = Join-Path $Project (Join-Path 'models' (Join-Path $component.dir $file.path))
        if (Test-Path $local) { Copy-Item $local $target } else { Invoke-WebRequest $file.url -OutFile $target }
        if ((Get-FileHash $target -Algorithm SHA256).Hash -ne $file.sha256) { throw "$($file.path) differs from the catalog" }
    }
}

if ($Test) { cargo test --release --locked --workspace --tests }
cargo build --release --locked --bins

$Release = Join-Path $env:CARGO_TARGET_DIR 'release'
if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
New-Item -ItemType Directory -Force $Out | Out-Null
# The GPU pack's DLLs are the ones the catalog lists, downloaded when wanted; TensorRT is never used, nor
# the rest of NVIDIA's DLLs that build.rs puts beside the binaries.
$catalog = Get-Content (Join-Path $Project 'crates\core\catalog.json') -Raw | ConvertFrom-Json
$gpuPack = ($catalog.components | Where-Object id -eq 'gpu-pack').files.path + 'onnxruntime_providers_tensorrt.dll'
$gpuPack += Get-ChildItem (Join-Path $Project 'third_party\cuda-runtime') -Filter '*.dll' -ErrorAction SilentlyContinue | ForEach-Object Name
$dlls = Get-ChildItem $Release -Filter '*.dll' | Where-Object Name -notin $gpuPack
foreach ($file in @('kiku.exe', 'kiku-mcp.exe', 'kiku-engine.exe' | ForEach-Object { Get-Item (Join-Path $Release $_) }) + $dlls) {
    Copy-Item $file.FullName $Out
}
# The Visual C++ runtime, app-local, since a fresh Windows may lack it: the newest copy here, as ONNX
# Runtime needs one at least as new as the compiler it was built with.
foreach ($name in 'msvcp140.dll', 'msvcp140_1.dll', 'msvcp140_2.dll', 'vcruntime140.dll', 'vcruntime140_1.dll') {
    $copies = @(Get-Item (Join-Path $env:SystemRoot "System32\$name")) + @(Get-ChildItem "${env:ProgramFiles}*\Microsoft Visual Studio\*\*\VC\Redist\MSVC\*\$Arch\Microsoft.VC14*.CRT\$name")
    $newest = $copies | Sort-Object { $v = $_.VersionInfo; [version]::new($v.FileMajorPart, $v.FileMinorPart, $v.FileBuildPart, $v.FilePrivatePart) } | Select-Object -Last 1
    Copy-Item $newest.FullName $Out
}
Save-Component $catalog 'silero-vad' (Join-Path $Out 'models')
Copy-Item (Join-Path $Project 'LICENSE') (Join-Path $Out 'LICENSE.txt')
python (Join-Path $PSScriptRoot 'notices.py') (Join-Path $Out 'THIRD-PARTY-NOTICES.txt')
"Staged in $Out"
