<#
.SYNOPSIS
    Installs ggml-rpc-server (llama.cpp build b12014, Windows Vulkan x64) for SAIL Mesh pooled compute.
    Opens no firewall port and requires no administrator rights.
#>
[CmdletBinding()]
param(
    [string]$CacheDir = "D:\sail-cache"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# Pinned llama.cpp release b12014 (Windows Vulkan x64) — verified against GitHub asset digest.
$ReleaseTag    = "b12014"
$ZipUrl        = "https://github.com/ggml-org/llama.cpp/releases/download/b12014/llama-b12014-bin-win-vulkan-x64.zip"
$ExpectedSha256 = "e3712df80ab21c81051f43ac6edd9f4c8faefb8f7f760d971a578a78d6bbbdac"

if ([string]::IsNullOrWhiteSpace($ExpectedSha256)) {
    throw "Refusing to install: ExpectedSha256 is empty (fail closed)."
}

if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
    throw "LOCALAPPDATA environment variable is not set."
}

$InstallDir = Join-Path $env:LOCALAPPDATA "SAIL\mesh\rpc"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$TempZip = Join-Path ([System.IO.Path]::GetTempPath()) ("llama-$ReleaseTag-win-vulkan-x64-" + [System.Guid]::NewGuid().ToString("N") + ".zip")

try {
    Write-Host "Downloading $ZipUrl ..."
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $ZipUrl -OutFile $TempZip -UseBasicParsing

    $ActualSha256 = (Get-FileHash -Path $TempZip -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($ActualSha256 -ne $ExpectedSha256.ToLowerInvariant()) {
        throw "SHA-256 mismatch for $ZipUrl! Expected $ExpectedSha256, got $ActualSha256. Aborting."
    }
    Write-Host "SHA-256 verified ($ActualSha256)."

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $ExactFiles = @(
        "ggml-rpc-server.exe",
        "ggml.dll",
        "ggml-base.dll",
        "ggml-rpc.dll",
        "ggml-vulkan.dll",
        "libomp.dll"
    )

    $Archive = [System.IO.Compression.ZipFile]::OpenRead($TempZip)
    try {
        $Extracted = @()
        foreach ($Entry in $Archive.Entries) {
            if ([string]::IsNullOrEmpty($Entry.Name)) { continue }
            $Name = $Entry.Name
            $IsNeeded = ($ExactFiles -contains $Name) -or ($Name -like "ggml-cpu-*.dll")
            if (-not $IsNeeded) { continue }

            $DestPath = Join-Path $InstallDir $Name
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($Entry, $DestPath, $true)
            $Extracted += $Name
        }
    } finally {
        $Archive.Dispose()
    }

    $RpcExe = Join-Path $InstallDir "ggml-rpc-server.exe"
    if (-not (Test-Path $RpcExe)) {
        throw "ggml-rpc-server.exe was not found in the downloaded archive."
    }

    Write-Host ""
    Write-Host "Installed $($Extracted.Count) files into $InstallDir"
    Write-Host "Run the worker with an HDD weight cache (no firewall port opened):"
    Write-Host ""
    Write-Host "  sail-mesh-worker serve --compute `"$RpcExe`" --cache-dir `"$CacheDir`""
    Write-Host ""
} finally {
    if (Test-Path $TempZip) {
        Remove-Item -Force -Path $TempZip -ErrorAction SilentlyContinue
    }
}
