function New-PointoryArchive {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$SourceDirectory,
        [Parameter(Mandatory)][string]$ArchivePath,
        [Parameter(Mandatory)][string]$ReleaseName
    )

    # A used portable directory may contain personal tasks and a lock file.
    # Release archives contain only these explicitly named distribution files.
    $distributionFiles = @('Pointory.exe', 'README.md', 'CHANGELOG.md')
    foreach ($filename in $distributionFiles) {
        if (-not (Test-Path -LiteralPath (Join-Path $SourceDirectory $filename) -PathType Leaf)) {
            throw "发布文件不存在：$filename"
        }
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archiveAbsolute = [System.IO.Path]::GetFullPath($ArchivePath)
    $temporaryArchive = "$archiveAbsolute.$([guid]::NewGuid().ToString('N')).tmp"
    try {
        $zip = [System.IO.Compression.ZipFile]::Open($temporaryArchive, [System.IO.Compression.ZipArchiveMode]::Create)
        try {
            foreach ($filename in $distributionFiles) {
                [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                    $zip,
                    [System.IO.Path]::GetFullPath((Join-Path $SourceDirectory $filename)),
                    "$ReleaseName/$filename",
                    [System.IO.Compression.CompressionLevel]::Optimal
                ) | Out-Null
            }
        }
        finally { $zip.Dispose() }
        [System.IO.File]::Move($temporaryArchive, $archiveAbsolute, $true)
    }
    finally {
        if (Test-Path -LiteralPath $temporaryArchive -PathType Leaf) {
            Remove-Item -LiteralPath $temporaryArchive -Force
        }
    }
}
