& {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = "Stop"

    if ($PSVersionTable.PSVersion.Major -lt 6) {
        [Net.ServicePointManager]::SecurityProtocol =
            [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    }

    $Version = $env:OPSAIL_VERSION
    $InstallDir = $env:OPSAIL_INSTALL_DIR

    if ([string]::IsNullOrWhiteSpace($Version)) {
        $Version = "latest"
    }

    if ([string]::IsNullOrWhiteSpace($InstallDir)) {
        $InstallDir = Join-Path $HOME ".local\bin"
    }

    $InstallDir = [IO.Path]::GetFullPath($InstallDir)
    if ($InstallDir.IndexOfAny([char[]]"`r`n;") -ge 0) {
        throw "opsail installer: install directory contains an invalid character"
    }

    $architecture = if ($env:PROCESSOR_ARCHITEW6432) {
        $env:PROCESSOR_ARCHITEW6432
    } else {
        $env:PROCESSOR_ARCHITECTURE
    }

    if ($architecture -ne "AMD64") {
        throw "opsail installer: unsupported Windows architecture: $architecture"
    }

    $target = "x86_64-pc-windows-msvc"
    $asset = "opsail-$target.zip"

    if ($Version -eq "latest") {
        $releaseUrl = "https://github.com/lencx/opsail/releases/latest/download"
    } else {
        if ($Version -notmatch "^[0-9A-Za-z._-]+$") {
            throw "opsail installer: invalid version: $Version"
        }

        $tag = if ($Version.StartsWith("v")) { $Version } else { "v$Version" }
        $releaseUrl = "https://github.com/lencx/opsail/releases/download/$tag"
    }

    $tempDir = Join-Path ([IO.Path]::GetTempPath()) ("opsail-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Force $tempDir | Out-Null

    try {
        $archivePath = Join-Path $tempDir $asset
        $checksumsPath = Join-Path $tempDir "SHA256SUMS"

        Write-Host "Downloading opsail for $target..."
        Invoke-WebRequest -UseBasicParsing -Uri "$releaseUrl/$asset" -OutFile $archivePath
        Invoke-WebRequest -UseBasicParsing -Uri "$releaseUrl/SHA256SUMS" -OutFile $checksumsPath

        $expectedHash = $null
        foreach ($line in Get-Content -LiteralPath $checksumsPath) {
            if ($line -match "^([A-Fa-f0-9]{64})\s+\*?(.+)$" -and $Matches[2] -eq $asset) {
                $expectedHash = $Matches[1].ToLowerInvariant()
                break
            }
        }

        if (-not $expectedHash) {
            throw "opsail installer: checksum not found for $asset"
        }

        $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
        if ($actualHash -ne $expectedHash) {
            throw "opsail installer: checksum verification failed for $asset"
        }

        Expand-Archive -LiteralPath $archivePath -DestinationPath $tempDir -Force
        $binaryPath = Join-Path (Join-Path $tempDir "opsail-$target") "opsail.exe"
        if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
            throw "opsail installer: downloaded archive does not contain opsail.exe"
        }

        & $binaryPath --version
        if ($LASTEXITCODE -ne 0) {
            throw "opsail installer: downloaded opsail binary exited with code $LASTEXITCODE"
        }

        New-Item -ItemType Directory -Force $InstallDir | Out-Null
        $installedBinary = Join-Path $InstallDir "opsail.exe"
        Copy-Item -LiteralPath $binaryPath -Destination $installedBinary -Force

        $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
        $userPathEntries = @($userPath -split ";" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        if ($userPathEntries -notcontains $InstallDir) {
            [Environment]::SetEnvironmentVariable("Path", (($userPathEntries + $InstallDir) -join ";"), "User")
        }

        $currentPathEntries = @($env:Path -split ";")
        if ($currentPathEntries -notcontains $InstallDir) {
            $env:Path = "$InstallDir;$env:Path"
        }

        Write-Host "Installed opsail to $installedBinary"
    } finally {
        Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
