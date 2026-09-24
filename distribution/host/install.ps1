$ErrorActionPreference = 'Stop'

$baseUrl = if ($env:BAKE_RELEASE_BASE_URL) { $env:BAKE_RELEASE_BASE_URL.TrimEnd('/') } else { 'https://bake.justar.dev' }
$installRoot = if ($env:BAKE_INSTALL_ROOT) { $env:BAKE_INSTALL_ROOT } else { Join-Path $env:LOCALAPPDATA 'Bake' }
$binDir = if ($env:BAKE_BIN_DIR) { $env:BAKE_BIN_DIR } else { Join-Path $installRoot 'bin' }

if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
  throw 'This Windows platform has no Bake release archive.'
}
try { $nodeMajor = [int]((& node -p 'process.versions.node').Trim().Split('.')[0]) }
catch { throw 'Bake install needs Node.js 24 or newer on PATH.' }
if ($nodeMajor -lt 24) { throw 'Bake install needs Node.js 24 or newer on PATH.' }
if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) { throw 'Bake install needs Windows tar.exe.' }

$manifest = Invoke-RestMethod "$baseUrl/latest.json"
$version = [string]$manifest.version
$artifact = $manifest.artifacts.'win32-x64'
if ($version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$' -or
    -not $artifact -or
    [string]$artifact.file -ne "bake-v$version-win32-x64.tar.gz" -or
    [string]$artifact.sha256 -notmatch '^[a-f0-9]{64}$') {
  throw 'No valid Bake archive for win32-x64.'
}

$temporary = Join-Path ([IO.Path]::GetTempPath()) ("bake-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
  $archive = Join-Path $temporary $artifact.file
  Invoke-WebRequest "$baseUrl/releases/$version/$($artifact.file)" -OutFile $archive
  $actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
  if ($actual -ne $artifact.sha256) { throw 'Bake download checksum did not match the release manifest.' }
  $unpacked = Join-Path $temporary 'unpacked'
  New-Item -ItemType Directory -Path $unpacked | Out-Null
  & tar.exe -xzf $archive -C $unpacked
  if ($LASTEXITCODE -ne 0) { throw 'Could not extract the Bake archive.' }
  & node (Join-Path $unpacked 'apps\cli\lib\bin.js') --version | Out-Null
  if ($LASTEXITCODE -ne 0) { throw 'The Bake archive did not start.' }

  $versions = Join-Path $installRoot 'versions'
  New-Item -ItemType Directory -Force -Path $versions, $binDir | Out-Null
  $digestPrefix = ([string]$artifact.sha256).Substring(0, 12)
  $versionPath = Join-Path $versions "$version-$digestPrefix"
  if (Test-Path $versionPath) {
    & node (Join-Path $versionPath 'apps\cli\lib\bin.js') --version | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'The existing Bake installation did not start.' }
  } else {
    Move-Item $unpacked $versionPath
  }
  $cmd = Join-Path $binDir 'bake.cmd'
  $commandText = "@echo off`r`nif not defined DSH_HOME set `"DSH_HOME=%USERPROFILE%\.bake`"`r`n"
  $commandText += "if defined BAKE_INSTALL_ROOT (set `"BAKE_RELEASE_ROOT=%BAKE_INSTALL_ROOT%`") else (set `"BAKE_RELEASE_ROOT=%LOCALAPPDATA%\Bake`")`r`n"
  $commandText += "set `"BAKE_CLI=%BAKE_RELEASE_ROOT%\versions\$version-$digestPrefix\apps\cli\lib\bin.js`"`r`n"
  $commandText += "if /I `"%~1`"==`"tui`" goto raw`r`nif /I `"%~1`"==`"headless`" goto raw`r`n"
  $commandText += "if /I `"%~1`"==`"plugin`" goto raw`r`nif /I `"%~1`"==`"--profile`" goto raw`r`n"
  $commandText += "node `"%BAKE_CLI%`" --profile tui %*`r`nexit /b %ERRORLEVEL%`r`n:raw`r`nnode `"%BAKE_CLI%`" %*`r`nexit /b %ERRORLEVEL%"
  Set-Content -Path $cmd -Encoding Ascii -Value $commandText
  if ($env:BAKE_SKIP_PATH_UPDATE -ne '1') {
    $pathParts = [Environment]::GetEnvironmentVariable('Path', 'User') -split ';'
    if ($pathParts -notcontains $binDir) {
      $existing = [Environment]::GetEnvironmentVariable('Path', 'User')
      [Environment]::SetEnvironmentVariable('Path', (($existing.TrimEnd(';') + ';' + $binDir).TrimStart(';')), 'User')
    }
  }
  $env:Path += ";$binDir"
  Write-Output "Installed Bake $version at $versionPath. Run: bake"
} finally {
  Remove-Item -Recurse -Force $temporary -ErrorAction SilentlyContinue
}
