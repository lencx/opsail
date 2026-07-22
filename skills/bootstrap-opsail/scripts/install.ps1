# Canonical Opsail installer shared by the README and bootstrap skill.
[CmdletBinding()]
param(
    [string] $Version = $env:OPSAIL_VERSION,
    [string] $InstallDir = $env:OPSAIL_INSTALL_DIR,
    [switch] $UpdatePath
)

$UpdatePathWasBound = $PSBoundParameters.ContainsKey("UpdatePath")

& {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = "Stop"

    if ($PSVersionTable.PSVersion.Major -lt 6) {
        [Net.ServicePointManager]::SecurityProtocol =
            [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    }

    $ShouldUpdatePath = if ($UpdatePathWasBound) {
        $UpdatePath.IsPresent
    } else {
        [string]::Equals(
            $env:OPSAIL_UPDATE_PATH,
            "1",
            [StringComparison]::Ordinal
        )
    }

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

    function ConvertTo-PathKey {
        param([string] $Entry)

        if ([string]::IsNullOrWhiteSpace($Entry)) {
            return $null
        }

        $candidate = $Entry.Trim()
        if (
            $candidate.Length -ge 2 -and
            $candidate[0] -eq '"' -and
            $candidate[$candidate.Length - 1] -eq '"'
        ) {
            $candidate = $candidate.Substring(1, $candidate.Length - 2)
        }

        $candidate = [Environment]::ExpandEnvironmentVariables($candidate)
        try {
            $candidate = [IO.Path]::GetFullPath($candidate)
        } catch {
            # Preserve unusual PATH entries; they still receive case-insensitive
            # and trailing-separator normalization for comparison.
        }

        $root = [IO.Path]::GetPathRoot($candidate)
        $rootLength = if ([string]::IsNullOrEmpty($root)) { 0 } else { $root.Length }
        while (
            $candidate.Length -gt $rootLength -and
            ($candidate.EndsWith('\') -or $candidate.EndsWith('/'))
        ) {
            $candidate = $candidate.Substring(0, $candidate.Length - 1)
        }

        return $candidate
    }

    function Test-PathListContains {
        param(
            [AllowNull()] [string] $PathValue,
            [string] $RequiredPath
        )

        $requiredKey = ConvertTo-PathKey $RequiredPath
        foreach ($entry in @($PathValue -split ";")) {
            $entryKey = ConvertTo-PathKey $entry
            if (
                $null -ne $entryKey -and
                [string]::Equals($entryKey, $requiredKey, [StringComparison]::OrdinalIgnoreCase)
            ) {
                return $true
            }
        }

        return $false
    }

    function Add-NormalizedPathEntry {
        param(
            [AllowNull()] [string] $PathValue,
            [string] $RequiredPath
        )

        $requiredKey = ConvertTo-PathKey $RequiredPath
        $entries = [Collections.Generic.List[string]]::new()
        $foundRequiredPath = $false

        foreach ($entry in @($PathValue -split ";")) {
            if ([string]::IsNullOrWhiteSpace($entry)) {
                continue
            }

            $trimmedEntry = $entry.Trim()
            $entryKey = ConvertTo-PathKey $trimmedEntry
            $isRequiredPath =
                $null -ne $entryKey -and
                [string]::Equals($entryKey, $requiredKey, [StringComparison]::OrdinalIgnoreCase)

            if ($isRequiredPath) {
                if (-not $foundRequiredPath) {
                    [void] $entries.Add($RequiredPath)
                    $foundRequiredPath = $true
                }
            } else {
                [void] $entries.Add($trimmedEntry)
            }
        }

        if (-not $foundRequiredPath) {
            [void] $entries.Add($RequiredPath)
        }

        return ($entries -join ";")
    }

    $architecture = if ($env:PROCESSOR_ARCHITEW6432) {
        $env:PROCESSOR_ARCHITEW6432
    } else {
        $env:PROCESSOR_ARCHITECTURE
    }

    $target = switch ($architecture.ToUpperInvariant()) {
        "AMD64" { "x86_64-pc-windows-msvc"; break }
        "ARM64" { "aarch64-pc-windows-msvc"; break }
        default { throw "opsail installer: unsupported Windows architecture: $architecture" }
    }

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

        $downloadedVersionLines = @(& $binaryPath --version)
        $downloadedVersionExitCode = $LASTEXITCODE
        if ($downloadedVersionExitCode -ne 0) {
            throw "opsail installer: downloaded opsail binary exited with code $downloadedVersionExitCode"
        }
        $downloadedVersion = ($downloadedVersionLines -join [Environment]::NewLine).Trim()
        $downloadedVersionMatch = [Text.RegularExpressions.Regex]::Match(
            $downloadedVersion,
            '\Aopsail ([0-9A-Za-z.+_-]+)\z',
            [Text.RegularExpressions.RegexOptions]::CultureInvariant
        )
        if (-not $downloadedVersionMatch.Success) {
            throw "opsail installer: downloaded opsail returned malformed version output: $downloadedVersion"
        }
        $reportedVersion = $downloadedVersionMatch.Groups[1].Value

        if ($Version -ne "latest") {
            $expectedVersion = if ($Version.StartsWith("v", [StringComparison]::Ordinal)) {
                $Version.Substring(1)
            } else {
                $Version
            }
            if (-not [string]::Equals(
                $reportedVersion,
                $expectedVersion,
                [StringComparison]::Ordinal
            )) {
                throw "opsail installer: downloaded opsail version mismatch: expected $expectedVersion, found $reportedVersion"
            }
        }

        New-Item -ItemType Directory -Force $InstallDir | Out-Null
        $installedBinary = Join-Path $InstallDir "opsail.exe"
        Copy-Item -LiteralPath $binaryPath -Destination $installedBinary -Force

        $installedVersionLines = @(& $installedBinary --version)
        $installedVersionExitCode = $LASTEXITCODE
        if ($installedVersionExitCode -ne 0) {
            throw "opsail installer: installed opsail binary exited with code $installedVersionExitCode"
        }
        $installedVersion = ($installedVersionLines -join [Environment]::NewLine).Trim()

        if (-not [string]::Equals(
            $installedVersion,
            $downloadedVersion,
            [StringComparison]::Ordinal
        )) {
            throw "opsail installer: installed opsail version mismatch: expected $downloadedVersion, found $installedVersion"
        }

        Write-Host $installedVersion

        if ($ShouldUpdatePath) {
            $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
            $normalizedUserPath = Add-NormalizedPathEntry $userPath $InstallDir
            if (-not [string]::Equals($userPath, $normalizedUserPath, [StringComparison]::Ordinal)) {
                [Environment]::SetEnvironmentVariable("Path", $normalizedUserPath, "User")
            }

            $env:Path = Add-NormalizedPathEntry $env:Path $InstallDir
        } elseif (-not (Test-PathListContains $env:Path $InstallDir)) {
            Write-Host "The install directory is not on PATH: $InstallDir"
            Write-Host "Add that directory to your user PATH, or rerun with OPSAIL_UPDATE_PATH=1."
        }

        Write-Host "Installed opsail to $installedBinary"
    } finally {
        Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
