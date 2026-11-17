# Installs the fsend CLI from the latest GitHub release, or removes it.
#
#   irm https://fsend.sh/install.ps1 | iex
#   iex "& {$(irm https://fsend.sh/install.ps1)} -Uninstall"
param(
    # Where fsend.exe goes; it is added to the user's PATH.
    [string]$Dir = "$env:LOCALAPPDATA\Programs\fsend",
    [switch]$Uninstall
)

# A script block, so the settings below don't leak into the caller's session.
& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    Set-StrictMode -Version Latest
    # Windows PowerShell 5.1 may still offer TLS 1.0, which GitHub refuses.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $Releases = 'https://github.com/maxomatic458/fsend/releases/latest/download'
    # Also runs on ARM64 Windows, through its x64 emulation.
    $Target = 'x86_64-pc-windows-msvc'
    # Where earlier versions of this script installed to.
    $Legacy = "$env:LOCALAPPDATA\fsend"

    function Get-UserPath {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
        try {
            # Unexpanded, so entries like %USERPROFILE%\bin stay as they are.
            "$($key.GetValue('Path', '', 'DoNotExpandEnvironmentNames'))" -split ';' | Where-Object { $_ }
        } finally {
            $key.Close()
        }
    }

    function Set-UserPath([string[]]$Entries) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
        try {
            $key.SetValue('Path', ($Entries -join ';'), 'ExpandString')
        } finally {
            $key.Close()
        }
        # Setting a variable through .NET tells running programs, like Explorer, that PATH changed.
        [Environment]::SetEnvironmentVariable('FSEND_INSTALL', '1', 'User')
        [Environment]::SetEnvironmentVariable('FSEND_INSTALL', $null, 'User')
    }

    function Test-SameDir([string]$A, [string]$B) {
        [Environment]::ExpandEnvironmentVariables($A).TrimEnd('\') -eq $B.TrimEnd('\')
    }

    # Drops $Dirs from the user's and this session's PATH.
    function Remove-FromPath([string[]]$Dirs) {
        $isListed = { param($Entry) foreach ($d in $Dirs) { if (Test-SameDir $Entry $d) { return $true } } $false }
        $entries = @(Get-UserPath)
        $kept = @($entries | Where-Object { -not (& $isListed $_) })
        if ($kept.Count -ne $entries.Count) { Set-UserPath $kept }
        $env:Path = (($env:Path -split ';') | Where-Object { $_ -and -not (& $isListed $_) }) -join ';'
    }

    function Remove-Legacy {
        if (Test-Path $Legacy) { Remove-Item $Legacy -Recurse -Force }
        Remove-FromPath @("$Legacy\fsend-$Target")
    }

    function Install-Fsend {
        $archive = "fsend-$Target.zip"
        $tmp = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
        New-Item -ItemType Directory -Path $tmp | Out-Null
        try {
            Write-Host "fsend: downloading $archive"
            Invoke-WebRequest "$Releases/$archive" -OutFile "$tmp\$archive" -UseBasicParsing
            Invoke-WebRequest "$Releases/$archive.sha256" -OutFile "$tmp\$archive.sha256" -UseBasicParsing
            $expected = (Get-Content "$tmp\$archive.sha256" -Raw).Trim() -split '\s+' | Select-Object -First 1
            if ((Get-FileHash "$tmp\$archive" -Algorithm SHA256).Hash -ne $expected) {
                throw "fsend: $archive does not match its checksum"
            }

            Expand-Archive "$tmp\$archive" -DestinationPath $tmp
            New-Item -ItemType Directory -Path $Dir -Force | Out-Null
            Copy-Item "$tmp\fsend-$Target\fsend.exe" $Dir -Force
        } finally {
            Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
        }

        Remove-Legacy
        if (-not (Get-UserPath | Where-Object { Test-SameDir $_ $Dir })) {
            Set-UserPath (@(Get-UserPath) + $Dir)
        }
        if (-not (($env:Path -split ';') | Where-Object { Test-SameDir $_ $Dir })) {
            $env:Path = "$env:Path;$Dir"
        }
        $version = & "$Dir\fsend.exe" version | Select-Object -First 1
        Write-Host "fsend: installed $version to $Dir\fsend.exe"
    }

    function Uninstall-Fsend {
        if (Test-Path "$Dir\fsend.exe") {
            Remove-Item "$Dir\fsend.exe" -Force
            # Only the folder this script made, never one the user chose that holds other files.
            if (-not (Get-ChildItem $Dir -Force)) { Remove-Item $Dir -Force }
            Write-Host "fsend: removed $Dir\fsend.exe"
        } else {
            Write-Host "fsend: nothing to remove at $Dir\fsend.exe"
        }
        Remove-FromPath @($Dir)
        Remove-Legacy
    }

    if ($Uninstall) { Uninstall-Fsend } else { Install-Fsend }
}
