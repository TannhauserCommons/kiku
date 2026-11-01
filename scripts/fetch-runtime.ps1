# Fetches the runtime every model shares into third_party/:
#   sherpa-onnx/lib  sherpa-onnx's C API and its CUDA-enabled ONNX Runtime 1.28 (Moonshine links it too)
#   cuda-runtime     NVIDIA's redistributable CUDA 12 and cuDNN 9 DLLs, so the GPU option needs no install.
#                    No cuFFT: ONNX Runtime loads it only for FFT operators, which no model here uses.
# About 3 GB of downloads. Re-running skips what is already there.
#   -CpuOnly   skips NVIDIA's DLLs, which release builds don't ship (the app downloads them).
param([switch]$CpuOnly)
$ErrorActionPreference = 'Stop'

$Root = Join-Path $PSScriptRoot '..\third_party'
$Sherpa = 'https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1'
$Nvidia = 'https://developer.download.nvidia.com/compute'
$NvidiaArchives = @(
    "$Nvidia/cuda/redist/cuda_cudart/windows-x86_64/cuda_cudart-windows-x86_64-12.9.79-archive.zip",
    "$Nvidia/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-12.9.1.4-archive.zip",
    "$Nvidia/cudnn/redist/cudnn/windows-x86_64/cudnn-windows-x86_64-9.27.0.42_cuda12-archive.zip"
)

function Get-Archive($url) {
    $file = Join-Path $Root (Split-Path $url -Leaf)
    if (-not (Test-Path $file)) { Invoke-WebRequest $url -OutFile $file }
    $file
}

$lib = Join-Path $Root 'sherpa-onnx\lib'
if (-not (Test-Path (Join-Path $lib 'onnxruntime_providers_cuda.dll'))) {
    New-Item -ItemType Directory -Force $lib | Out-Null
    # The CUDA package has every DLL; the plain package adds onnxruntime.lib, which Moonshine links against.
    foreach ($package in 'sherpa-onnx-v2.0.1-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-win-x64-cuda', 'sherpa-onnx-v2.0.1-win-x64-shared-MD-Release-no-tts-lib') {
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
