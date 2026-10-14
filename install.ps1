# Installs aether from its latest release, or from $env:AETHER_VERSION (e.g. v0.1.1):
#   irm https://raw.githubusercontent.com/tarikee/aether/main/install.ps1 | iex
# It goes to $env:AETHER_INSTALL_DIR, by default %LOCALAPPDATA%\Programs\aether, which is added
# to your PATH. Running it again updates aether.
& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = 'tarikee/aether'
    $name = 'aether-windows-x86_64'
    if (-not [Environment]::Is64BitOperatingSystem) {
        throw "aether install: no ready program for 32-bit Windows; build it with: cargo install --git https://github.com/$repo"
    }
    $version = if ($env:AETHER_VERSION) { $env:AETHER_VERSION } else { 'latest' }
    $base = if ($version -eq 'latest') {
        "https://github.com/$repo/releases/latest/download"
    } else {
        "https://github.com/$repo/releases/download/$version"
    }
    $dir = if ($env:AETHER_INSTALL_DIR) { $env:AETHER_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\aether' }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
    New-Item -ItemType Directory $tmp | Out-Null
    try {
        Write-Host "Downloading $name ($version)..."
        $zip = Join-Path $tmp "$name.zip"
        $sums = Join-Path $tmp 'SHA256SUMS'
        Invoke-WebRequest "$base/$name.zip" -OutFile $zip -UseBasicParsing
        Invoke-WebRequest "$base/SHA256SUMS" -OutFile $sums -UseBasicParsing
        $line = Get-Content $sums | Where-Object { $_ -match " $name\.zip$" } | Select-Object -First 1
        if (-not $line) { throw "aether install: SHA256SUMS has no line for $name.zip" }
        $want = ($line -split '\s+')[0]
        $got = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($got -ne $want) { throw "aether install: $name.zip does not match SHA256SUMS" }

        Expand-Archive $zip -DestinationPath (Join-Path $tmp 'x') -Force
        New-Item -ItemType Directory -Force $dir | Out-Null
        $exe = Join-Path $dir 'aether.exe'
        Copy-Item (Join-Path $tmp 'x\aether.exe') $exe -Force
        Unblock-File $exe
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = if ($userPath) { $userPath -split ';' } else { @() }
    if ($parts -notcontains $dir) {
        $joined = if ($userPath) { "$userPath;$dir" } else { $dir }
        [Environment]::SetEnvironmentVariable('Path', $joined, 'User')
        Write-Host "Added $dir to your PATH."
    }
    if (($env:Path -split ';') -notcontains $dir) { $env:Path = "$env:Path;$dir" }

    Write-Host "Installed $(& $exe version) to $exe"
    Write-Host 'Type aether to start it (in a new terminal if this one does not find it).'
}
