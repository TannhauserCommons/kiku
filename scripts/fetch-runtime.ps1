# Fetches the runtime every model shares into third_party/:
#   sherpa-onnx/lib  sherpa-onnx's C API and its CUDA-enabled ONNX Runtime 1.28 (Moonshine links it too)
#   cuda-runtime     NVIDIA's redistributable CUDA 12 and cuDNN 9 DLLs, so the GPU option needs no install.
#                    No cuFFT: ONNX Runtime loads it only for FFT operators, which no model here uses.
# About 3 GB of downloads. Re-running skips what is already there.
# For this PC's architecture: x64, or ARM64, which has no NVIDIA GPUs and so gets the CPU runtime only.
#   -CpuOnly   skips NVIDIA's DLLs, which release builds don't ship (the app downloads them).
param([switch]$CpuOnly)
$ErrorActionPreference = 'Stop'

$Arch = "$([Runtime.InteropServices.RuntimeInformation]::OSArchitecture)".ToLower()
$Root = Join-Path $PSScriptRoot '..\third_party'
$Sherpa = 'https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1'
$Nvidia = 'https://developer.download.nvidia.com/compute'
$NvidiaArchives = @(
    "$Nvidia/cuda/redist/cuda_cudart/windows-x86_64/cuda_cudart-windows-x86_64-12.9.79-archive.zip",
    "$Nvidia/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-12.9.1.4-archive.zip",
    "$Nvidia/cudnn/redist/cudnn/windows-x86_64/cudnn-windows-x86_64-9.27.0.42_cuda12-archive.zip"
)

# sha256 of every archive: sherpa-onnx's from its release, NVIDIA's from its redistrib manifests.
$Sha256 = @{
    'sherpa-onnx-v2.0.1-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-win-x64-cuda.tar.bz2' = 'cd983fa95431a8a08555b15005e1effd0b771f161d30f7f3bd52d518da48f6b6'
    'sherpa-onnx-v2.0.1-win-x64-shared-MD-Release-no-tts-lib.tar.bz2' = '3aea6fc2360ac67b349f61b5cbe237f34b2bb53dc805aa4b3df7ab23ba7ab35c'
    'sherpa-onnx-v2.0.1-win-arm64-shared-MD-Release-no-tts-lib.tar.bz2' = '44c31e2da1b7005da3d29a24e88da9686d71d5368e0805f44a0a517302231f6e'
    'cuda_cudart-windows-x86_64-12.9.79-archive.zip' = '179e9c43b0735ffe67207b3da556eb5a0c50f3047961882b7657d3b822d34ef8'
    'libcublas-windows-x86_64-12.9.1.4-archive.zip' = 'd534d98b0b453a98914dbf3adf47d7e84b55037abf02f87466439e1dcef581ed'
    'cudnn-windows-x86_64-9.27.0.42_cuda12-archive.zip' = '93949680d6499ad0f17fc1abb131825c9dd41b460844513f608e840e0c8ce72b'
}

function Get-Archive($url) {
    $name = Split-Path $url -Leaf
    $file = Join-Path $Root $name
    if (-not (Test-Path $file)) { Invoke-WebRequest $url -OutFile $file }
    $actual = (Get-FileHash $file -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $Sha256[$name]) {
        Remove-Item $file
        throw "$name has sha256 $actual, expected $($Sha256[$name])"
    }
    $file
}

$lib = Join-Path $Root 'sherpa-onnx\lib'
# On x64 the CUDA package has every DLL; the plain package adds onnxruntime.lib, which Moonshine links against.
$packages = @("sherpa-onnx-v2.0.1-win-$Arch-shared-MD-Release-no-tts-lib")
if ($Arch -eq 'x64') { $packages = @('sherpa-onnx-v2.0.1-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-win-x64-cuda') + $packages }
else { $CpuOnly = $true }
if (-not (Test-Path (Join-Path $lib 'onnxruntime.lib'))) {
    New-Item -ItemType Directory -Force $lib | Out-Null
    foreach ($package in $packages) {
        tar -xjf (Get-Archive "$Sherpa/$package.tar.bz2") -C $Root "$package/lib"
        Get-ChildItem (Join-Path $Root "$package\lib") | Where-Object { -not (Test-Path (Join-Path $lib $_.Name)) } | Move-Item -Destination $lib
        Remove-Item -Recurse (Join-Path $Root $package)
    }
}

$cuda = Join-Path $Root 'cuda-runtime'
if (-not $CpuOnly -and -not (Test-Path (Join-Path $cuda 'cudnn64_9.dll'))) {
    New-Item -ItemType Directory -Force $cuda | Out-Null
    foreach ($url in $NvidiaArchives) {
        $zip = Get-Archive $url
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $archive = [IO.Compression.ZipFile]::OpenRead($zip)
        $archive.Entries | Where-Object { $_.FullName -match '/bin/(x64/)?[^/]+\.dll$' -and $_.Name -notmatch '^(cufftw|nvblas)' } | ForEach-Object {
            [IO.Compression.ZipFileExtensions]::ExtractToFile($_, (Join-Path $cuda $_.Name), $true)
        }
        $archive.Dispose()
    }
}
"Runtime ready in $Root"
