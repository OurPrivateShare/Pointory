$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'package-windows.ps1')

function Assert-PointoryPackage {
    param(
        [Parameter(Mandatory)][string]$ArchivePath,
        [Parameter(Mandatory)][string]$ReleaseName,
        [Parameter(Mandatory)][System.Collections.IDictionary]$ExpectedFiles
    )

    $zip = [System.IO.Compression.ZipFile]::OpenRead($ArchivePath)
    try {
        $expectedNames = @($ExpectedFiles.Keys | ForEach-Object { "$ReleaseName/$_" } | Sort-Object)
        $actualNames = @($zip.Entries | ForEach-Object { $_.FullName } | Sort-Object)
        if ($zip.Entries.Count -ne 3 -or
            [string]::Join("`n", $actualNames) -cne [string]::Join("`n", $expectedNames)) {
            throw "发布包包含了白名单以外的文件：$($actualNames -join ', ')"
        }
        foreach ($filename in $ExpectedFiles.Keys) {
            $entry = $zip.GetEntry("$ReleaseName/$filename")
            $reader = [System.IO.StreamReader]::new($entry.Open(), [System.Text.Encoding]::UTF8)
            try {
                if ($reader.ReadToEnd() -cne $ExpectedFiles[$filename]) {
                    throw "发布包文件未更新或内容不一致：$filename"
                }
            }
            finally { $reader.Dispose() }
        }
    }
    finally { $zip.Dispose() }
}

$fixtureName = "pointory-package-test-$([guid]::NewGuid().ToString('N'))"
$fixtureDirectory = [System.IO.Path]::GetFullPath((Join-Path ([System.IO.Path]::GetTempPath()) $fixtureName))
$archivePath = Join-Path $fixtureDirectory 'Pointory-test.zip'
$releaseName = 'Pointory-package-fixture-Windows-x64'
$distributionFiles = [ordered]@{
    'Pointory.exe' = 'dummy executable: first build'
    'README.md' = 'distribution readme'
    'CHANGELOG.md' = 'distribution changelog'
}
$privateFiles = [ordered]@{
    'config.json' = '{"private_tasks":["must stay in the portable directory"]}'
    'instance.lock' = 'private runtime lock'
    'private-notes.txt' = 'private notes must not enter a release archive'
}
$encoding = [System.Text.UTF8Encoding]::new($false)
$created = $false
try {
    New-Item -ItemType Directory -Path $fixtureDirectory | Out-Null
    $created = $true
    foreach ($files in @($distributionFiles, $privateFiles)) {
        foreach ($filename in $files.Keys) {
            [System.IO.File]::WriteAllText((Join-Path $fixtureDirectory $filename), $files[$filename], $encoding)
        }
    }

    New-PointoryArchive -SourceDirectory $fixtureDirectory -ArchivePath $archivePath -ReleaseName $releaseName
    Assert-PointoryPackage -ArchivePath $archivePath -ReleaseName $releaseName -ExpectedFiles $distributionFiles

    # Reusing an already-used portable directory must replace the old archive,
    # including its executable, while still excluding every runtime/private file.
    $distributionFiles['Pointory.exe'] = 'dummy executable: second build'
    [System.IO.File]::WriteAllText((Join-Path $fixtureDirectory 'Pointory.exe'), $distributionFiles['Pointory.exe'], $encoding)
    New-PointoryArchive -SourceDirectory $fixtureDirectory -ArchivePath $archivePath -ReleaseName $releaseName
    Assert-PointoryPackage -ArchivePath $archivePath -ReleaseName $releaseName -ExpectedFiles $distributionFiles

    foreach ($filename in $privateFiles.Keys) {
        $path = Join-Path $fixtureDirectory $filename
        if (-not [System.IO.File]::Exists($path) -or
            [System.IO.File]::ReadAllText($path, $encoding) -cne $privateFiles[$filename]) {
            throw "打包操作修改了便携目录中的私有文件：$filename"
        }
    }
    Write-Output '发布包白名单、覆盖更新和私有文件保留检查通过。'
}
finally {
    if ($created) {
        # Delete only the exact files created by this fixture. The directory
        # removal is explicitly non-recursive, so unknown contents are retained.
        $knownNames = @($distributionFiles.Keys) + @($privateFiles.Keys) + @('Pointory-test.zip')
        foreach ($filename in $knownNames) {
            [System.IO.File]::Delete((Join-Path $fixtureDirectory $filename))
        }
        [System.IO.Directory]::Delete($fixtureDirectory, $false)
    }
}
