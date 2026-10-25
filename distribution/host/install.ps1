# The Bake native installer for Windows: it installs the Rust agent, `bake`,
# from the signed native release channel. The host needs no Node or Bun; it
# needs Windows PowerShell 5.1 or newer and `tar.exe`.
#
#   irm https://bake.justar.dev/install.ps1 | iex
#
# Bake 0.4.0 publishes Linux releases only, so this script stops at its start,
# before it reads any setting or downloads anything; native Windows releases
# arrive in 0.4.x, and the rest of this script is the installer they use.
#
# See distribution/README.md, "Native installers", for the route, the install
# layout, the `bake` takeover, and the lock these steps take.
$ErrorActionPreference = 'Stop'

# BEGIN 0.4.0 PLATFORM REFUSAL
# Thrown, not `exit`: under `irm | iex` an exit would close the user's shell.
# The tests run the installer below through a copy without this block.
throw 'Bake install: Bake 0.4.0 runs on Linux only; Windows support is coming in a later 0.4 release. Until then, use WSL.'
# END 0.4.0 PLATFORM REFUSAL

# The native release key (distribution/host/native-release-key.pub), base64
# DER SubjectPublicKeyInfo. A release test holds every copy equal. Tests
# replace this one line in a private copy of the installer, so the published
# installer takes no key from its environment.
$nativeReleaseKey = 'MCowBQYDK2VwAyEAYHdFEaVkRABjXEFzF1WnfoX1MJUcwzgB9oS9kyzF9hY='

# BEGIN ED25519 VERIFIER
# Ed25519 verification (RFC 8032 section 5.1.7) in PowerShell, because neither
# Windows PowerShell nor Windows CNG has an Ed25519 primitive and the native
# installer runs no Node. Only public keys, signatures, and release bytes pass
# through here, so no secret is handled and no timing side channel matters.
# The tests extract this region and run it against RFC 8032's test vectors.
$script:BakeEd25519P = [System.Numerics.BigInteger]::Pow(2, 255) - 19
$script:BakeEd25519L = [System.Numerics.BigInteger]::Pow(2, 252) + [System.Numerics.BigInteger]::Parse('27742317777372353535851937790883648493')

function BakeFieldMod([System.Numerics.BigInteger]$value) {
  $residue = $value % $script:BakeEd25519P
  if ($residue.Sign -lt 0) { $residue = $residue + $script:BakeEd25519P }
  return $residue
}

function BakeFieldInverse([System.Numerics.BigInteger]$value) {
  return [System.Numerics.BigInteger]::ModPow((BakeFieldMod $value), $script:BakeEd25519P - 2, $script:BakeEd25519P)
}

# A little-endian byte string as a non-negative integer; the extra zero byte
# keeps BigInteger from reading the top bit as a sign.
function BakeLittleEndian([byte[]]$bytes) {
  $padded = New-Object byte[] ($bytes.Length + 1)
  [Array]::Copy($bytes, $padded, $bytes.Length)
  return New-Object System.Numerics.BigInteger (, $padded)
}

# The unary comma keeps an empty or one-element result an array: PowerShell
# would otherwise return $null or a bare byte.
function BakeHexBytes([string]$hex) {
  $bytes = New-Object byte[] ($hex.Length / 2)
  for ($index = 0; $index -lt $bytes.Length; $index++) {
    $bytes[$index] = [Convert]::ToByte($hex.Substring($index * 2, 2), 16)
  }
  return , $bytes
}

# Points are extended twisted Edwards coordinates (X, Y, Z, T), where the
# addition below is complete for this curve, so it doubles as well.
function BakePointAdd($left, $right) {
  $a = BakeFieldMod (($left[1] - $left[0]) * ($right[1] - $right[0]))
  $b = BakeFieldMod (($left[1] + $left[0]) * ($right[1] + $right[0]))
  $c = BakeFieldMod ($left[3] * 2 * $script:BakeEd25519D * $right[3])
  $d = BakeFieldMod ($left[2] * 2 * $right[2])
  $e = $b - $a
  $f = $d - $c
  $g = $d + $c
  $h = $b + $a
  return @((BakeFieldMod ($e * $f)), (BakeFieldMod ($g * $h)), (BakeFieldMod ($f * $g)), (BakeFieldMod ($e * $h)))
}

function BakeScalarMultiply($point, [System.Numerics.BigInteger]$scalar) {
  $result = @([System.Numerics.BigInteger]::Zero, [System.Numerics.BigInteger]::One, [System.Numerics.BigInteger]::One, [System.Numerics.BigInteger]::Zero)
  $addend = $point
  $remaining = $scalar
  while ($remaining -gt 0) {
    if (-not ($remaining % 2).IsZero) { $result = BakePointAdd $result $addend }
    $addend = BakePointAdd $addend $addend
    $remaining = $remaining / 2
  }
  return $result
}

function BakePointEquals($left, $right) {
  if ((BakeFieldMod ($left[0] * $right[2])) -ne (BakeFieldMod ($right[0] * $left[2]))) { return $false }
  return (BakeFieldMod ($left[1] * $right[2])) -eq (BakeFieldMod ($right[1] * $left[2]))
}

# RFC 8032 section 5.1.3: decompress 32 little-endian bytes into a point, or
# return $null for a non-canonical encoding (y at or above p) or a y with no x.
function BakePointDecode([byte[]]$bytes) {
  if ($null -eq $bytes -or $bytes.Length -ne 32) { return $null }
  $copy = New-Object byte[] 32
  [Array]::Copy($bytes, $copy, 32)
  $sign = ($copy[31] -shr 7) -band 1
  $copy[31] = $copy[31] -band 0x7f
  $y = BakeLittleEndian $copy
  if ($y -ge $script:BakeEd25519P) { return $null }
  $square = BakeFieldMod ($y * $y)
  $u = BakeFieldMod ($square - 1)
  $v = BakeFieldMod ($script:BakeEd25519D * $square + 1)
  $candidate = BakeFieldMod ($u * [System.Numerics.BigInteger]::ModPow($v, 3, $script:BakeEd25519P) *
    [System.Numerics.BigInteger]::ModPow((BakeFieldMod ($u * [System.Numerics.BigInteger]::ModPow($v, 7, $script:BakeEd25519P))),
      ($script:BakeEd25519P - 5) / 8, $script:BakeEd25519P))
  $check = BakeFieldMod ($v * $candidate * $candidate)
  if ($check -ne $u) {
    if ($check -eq (BakeFieldMod (-$u))) {
      $candidate = BakeFieldMod ($candidate * $script:BakeEd25519SqrtMinusOne)
    } else {
      return $null
    }
  }
  if ($candidate.IsZero -and $sign -eq 1) { return $null }
  if (($candidate % 2) -ne $sign) { $candidate = $script:BakeEd25519P - $candidate }
  return @($candidate, $y, [System.Numerics.BigInteger]::One, (BakeFieldMod ($candidate * $y)))
}

<#
.SYNOPSIS
Whether `Signature` is a valid Ed25519 signature of `Message` under `PublicKey`.
.DESCRIPTION
RFC 8032 section 5.1.7 with the cofactorless check [S]B = R + [k]A. A
signature whose S is at or above the group order, whose R or A is a
non-canonical or undecodable point, or whose lengths differ from 32 and 64
bytes is refused without any curve arithmetic.
#>
function Test-BakeEd25519Signature([byte[]]$PublicKey, [byte[]]$Signature, [byte[]]$Message) {
  if ($null -eq $PublicKey -or $PublicKey.Length -ne 32) { return $false }
  if ($null -eq $Signature -or $Signature.Length -ne 64) { return $false }
  if ($null -eq $Message) { return $false }
  $signatureR = New-Object byte[] 32
  $signatureS = New-Object byte[] 32
  [Array]::Copy($Signature, 0, $signatureR, 0, 32)
  [Array]::Copy($Signature, 32, $signatureS, 0, 32)
  $s = BakeLittleEndian $signatureS
  if ($s -ge $script:BakeEd25519L) { return $false }
  $publicPoint = BakePointDecode $PublicKey
  if ($null -eq $publicPoint) { return $false }
  $commitment = BakePointDecode $signatureR
  if ($null -eq $commitment) { return $false }
  $buffer = New-Object byte[] (64 + $Message.Length)
  [Array]::Copy($signatureR, 0, $buffer, 0, 32)
  [Array]::Copy($PublicKey, 0, $buffer, 32, 32)
  if ($Message.Length -gt 0) { [Array]::Copy($Message, 0, $buffer, 64, $Message.Length) }
  $sha512 = [System.Security.Cryptography.SHA512]::Create()
  try { $digest = $sha512.ComputeHash($buffer) } finally { $sha512.Dispose() }
  $k = (BakeLittleEndian $digest) % $script:BakeEd25519L
  $left = BakeScalarMultiply $script:BakeEd25519Base $s
  $right = BakePointAdd $commitment (BakeScalarMultiply $publicPoint $k)
  return (BakePointEquals $left $right)
}

# The curve constant d = -121665/121666, the square root of -1, and the base
# point, whose encoding is y = 4/5 with a cleared sign bit.
$script:BakeEd25519D = BakeFieldMod ([System.Numerics.BigInteger]::MinusOne * 121665 * (BakeFieldInverse 121666))
$script:BakeEd25519SqrtMinusOne = [System.Numerics.BigInteger]::ModPow(2, ($script:BakeEd25519P - 1) / 4, $script:BakeEd25519P)
$script:BakeEd25519Base = BakePointDecode (BakeHexBytes '5866666666666666666666666666666666666666666666666666666666666666')

# An Ed25519 public key from base64 DER SubjectPublicKeyInfo: the fixed
# 12-byte Ed25519 header and the 32 raw key bytes.
function BakeSpkiPublicKey([string]$base64) {
  $der = [Convert]::FromBase64String($base64.Trim())
  $header = BakeHexBytes '302a300506032b6570032100'
  if ($der.Length -ne 44) { return $null }
  for ($index = 0; $index -lt $header.Length; $index++) {
    if ($der[$index] -ne $header[$index]) { return $null }
  }
  $key = New-Object byte[] 32
  [Array]::Copy($der, 12, $key, 0, 32)
  return , $key
}
# END ED25519 VERIFIER

$downloadUrl = if ($env:BAKE_DOWNLOAD_URL) { $env:BAKE_DOWNLOAD_URL.TrimEnd('/') } else { 'https://bake.justar.dev' }
$localAppData = $env:LOCALAPPDATA
if (-not $localAppData) { $localAppData = [Environment]::GetFolderPath('LocalApplicationData') }
if (-not $env:BAKE_NATIVE_ROOT -and -not $localAppData) { throw 'Bake install: LOCALAPPDATA names no directory for the install root' }
$nativeRoot = if ($env:BAKE_NATIVE_ROOT) { $env:BAKE_NATIVE_ROOT } else { Join-Path $localAppData 'bake-native' }
$nativeRoot = [IO.Path]::GetFullPath($nativeRoot)
# The 0.3 TypeScript install, which this installer reads to recognize a
# Bake-managed `bake.cmd` and never writes to.
$legacyRoot = if ($env:BAKE_INSTALL_ROOT) { $env:BAKE_INSTALL_ROOT } elseif ($localAppData) { Join-Path $localAppData 'Bake' } else { '' }
$replaceCommand = $env:BAKE_REPLACE_COMMAND -eq '1'
$uninstallLinks = $false
foreach ($argument in $args) {
  switch -regex ("$argument") {
    '^(--|-)replace-?command$' { $replaceCommand = $true }
    '^(--|-)uninstall-?links$' { $uninstallLinks = $true }
    default { throw "Bake install: unknown option $argument" }
  }
}

# The command directory: the one a 0.3 install already put on PATH, so the
# takeover replaces the `bake` the user's PATH names, and otherwise the native
# install's own `bin`, which is added to the user PATH.
function BakeCommandDirectory() {
  if ($env:BAKE_BIN_DIR) { return [IO.Path]::GetFullPath($env:BAKE_BIN_DIR) }
  if ($legacyRoot -eq '') { return (Join-Path $nativeRoot 'bin') }
  $legacyBin = Join-Path $legacyRoot 'bin'
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if ((Test-Path -LiteralPath $legacyBin -PathType Container) -and $null -ne $userPath) {
    foreach ($part in ($userPath -split ';')) {
      if ($part.TrimEnd('\') -eq $legacyBin.TrimEnd('\')) { return $legacyBin }
    }
  }
  return (Join-Path $nativeRoot 'bin')
}

$binDir = BakeCommandDirectory
$lockDir = Join-Path $nativeRoot 'install.lock'
$lockHeld = $false
$temporary = $null

# `<root>\install.lock` is a directory, created atomically, holding one
# `owner` line of `<pid> <unix seconds> <kind>`. The Rust updater takes the
# same lock. A held lock whose owner process is gone is taken over once; a
# lock with no owner file counts as held for ten more seconds.
function BakeAcquireLock([string]$kind) {
  New-Item -ItemType Directory -Force -Path $nativeRoot | Out-Null
  for ($attempt = 0; $attempt -lt 2; $attempt++) {
    try {
      New-Item -ItemType Directory -Path $lockDir -ErrorAction Stop | Out-Null
      $script:lockHeld = $true
      $seconds = [long][Math]::Floor(([DateTimeOffset]::UtcNow).ToUnixTimeSeconds())
      [IO.File]::WriteAllText((Join-Path $lockDir 'owner'), "$PID $seconds $kind`n")
      return
    } catch {
      if ($attempt -gt 0 -or -not (BakeLockStale)) {
        throw "Bake install: another Bake install holds $lockDir; try again when it finishes"
      }
      Remove-Item -Recurse -Force -LiteralPath $lockDir -ErrorAction SilentlyContinue
    }
  }
}

function BakeLockStale() {
  $owner = Join-Path $lockDir 'owner'
  $waited = 0
  while (-not (Test-Path -LiteralPath $owner -PathType Leaf)) {
    if (-not (Test-Path -LiteralPath $lockDir -PathType Container)) { return $false }
    if ($waited -ge 10) { return $true }
    Start-Sleep -Seconds 1
    $waited++
  }
  $line = ''
  try { $line = (Get-Content -LiteralPath $owner -TotalCount 1 -ErrorAction Stop) } catch { return $true }
  $ownerPid = 0
  if (-not [int]::TryParse(("$line" -split '\s+')[0], [ref]$ownerPid)) { return $true }
  try {
    Get-Process -Id $ownerPid -ErrorAction Stop | Out-Null
    return $false
  } catch {
    return $true
  }
}

function BakeReleaseLock() {
  if ($script:lockHeld) {
    $script:lockHeld = $false
    Remove-Item -Recurse -Force -LiteralPath $lockDir -ErrorAction SilentlyContinue
  }
}

# A path as a JSON string's content for links.json: every quote and backslash
# becomes a `\uXXXX` escape, so an escaped value holds neither and a record
# can be read by splitting it on quotes.
function BakeJsonValue([string]$value) {
  return $value.Replace('\', '\u005c').Replace('"', '\u0022')
}

function BakeJsonPath([string]$value) {
  return $value.Replace('\u0022', '"').Replace('\u005c', '\')
}

# BEGIN VERSION ORDER
# Release versions by the SemVer precedence bake-update's `compare_versions`
# applies: a pre-release is older than its own release; identifiers compare
# one by one, numeric ones by value at any length, alphanumeric ones as
# ordinal text, a numeric one before an alphanumeric one, and a shorter list
# first when the other extends it. Numbers are compared as digit strings, so
# no [int] cast overflows on a number the grammar accepts.
# crates/bake-update/tests/fixtures/native-version-order.txt holds the cases every
# implementation is tested against.
function BakeCompareNumbers([string]$left, [string]$right) {
  $left = $left.TrimStart('0')
  $right = $right.TrimStart('0')
  if ($left.Length -ne $right.Length) { return [Math]::Sign($left.Length - $right.Length) }
  return [Math]::Sign([string]::CompareOrdinal($left, $right))
}

function BakeCompareVersions([string]$left, [string]$right) {
  $leftDash = $left.IndexOf('-')
  $rightDash = $right.IndexOf('-')
  $leftCore = if ($leftDash -ge 0) { $left.Substring(0, $leftDash) } else { $left }
  $rightCore = if ($rightDash -ge 0) { $right.Substring(0, $rightDash) } else { $right }
  $a = @($leftCore.Split('.'))
  $b = @($rightCore.Split('.'))
  for ($index = 0; $index -lt 3; $index++) {
    $x = if ($index -lt $a.Count -and $a[$index] -cmatch '^[0-9]+$') { $a[$index] } else { '0' }
    $y = if ($index -lt $b.Count -and $b[$index] -cmatch '^[0-9]+$') { $b[$index] } else { '0' }
    $order = BakeCompareNumbers $x $y
    if ($order -ne 0) { return $order }
  }
  if ($leftDash -lt 0 -and $rightDash -lt 0) { return 0 }
  if ($leftDash -lt 0) { return 1 }
  if ($rightDash -lt 0) { return -1 }
  $a = @($left.Substring($leftDash + 1).Split('.'))
  $b = @($right.Substring($rightDash + 1).Split('.'))
  for ($index = 0; $index -lt $a.Count -and $index -lt $b.Count; $index++) {
    $x = $a[$index]
    $y = $b[$index]
    $xNumber = $x -cmatch '^[0-9]+$'
    $yNumber = $y -cmatch '^[0-9]+$'
    $order = if ($xNumber -and $yNumber) { BakeCompareNumbers $x $y }
      elseif ($xNumber) { -1 }
      elseif ($yNumber) { 1 }
      else { [Math]::Sign([string]::CompareOrdinal($x, $y)) }
    if ($order -ne 0) { return $order }
  }
  return [Math]::Sign($a.Count - $b.Count)
}

function BakeVersionNewer([string]$candidate, [string]$installed) {
  return ((BakeCompareVersions $candidate $installed) -gt 0)
}
# END VERSION ORDER

# The release `current.txt` names, or an empty string when there is no pointer.
function BakeCurrentVersion() {
  $pointer = Join-Path $nativeRoot 'current.txt'
  if (-not (Test-Path -LiteralPath $pointer -PathType Leaf)) { return '' }
  try { return ([IO.File]::ReadAllText($pointer)).Trim() } catch { return '' }
}

# Replace a pointer file by writing a temporary file beside it and renaming,
# so a reader sees one release name or the other.
function BakeWritePointer([string]$name, [string]$version) {
  $pointer = Join-Path $nativeRoot $name
  # Named so that the sweep the Rust updater runs under the same lock, which
  # matches `.current*` and `.previous*`, clears an interrupted run's temp.
  $temp = Join-Path $nativeRoot ".$name.$PID.tmp"
  [IO.File]::WriteAllText($temp, "$version`r`n", (New-Object Text.UTF8Encoding $false))
  # `Move-Item -Force` renames over an existing file, which Windows does in
  # one step, so a reader sees one name or the other. `File.Replace` is not
  # used: PowerShell binds its $null backup argument as an empty path.
  Move-Item -Force -LiteralPath $temp -Destination $pointer
}

# The executable inside an unpacked release, which the archive holds as
# `bin\bake.exe`.
function BakeReleaseExecutable([string]$release) {
  $path = Join-Path (Join-Path $release 'bin') 'bake.exe'
  if (Test-Path -LiteralPath $path -PathType Leaf) { return $path }
  return ''
}

# The `bake.cmd` the installer writes: it reads the pointer on every run, so
# an update never rewrites the file a running `cmd.exe` is reading.
function BakeShimText([string]$root) {
  $literal = $root.Replace('%', '%%')
  return (@(
    '@echo off',
    'setlocal',
    "set ""BAKE_NATIVE_ROOT=$literal""",
    'set "BAKE_CURRENT="',
    'set /p BAKE_CURRENT=<"%BAKE_NATIVE_ROOT%\current.txt"',
    'if not defined BAKE_CURRENT (echo Bake is not installed correctly; run the installer again. 1>&2 & exit /b 1)',
    'set "BAKE_EXE=%BAKE_NATIVE_ROOT%\releases\%BAKE_CURRENT%\bin\bake.exe"',
    '"%BAKE_EXE%" %*',
    'exit /b %ERRORLEVEL%',
    ''
  ) -join "`r`n")
}

# Whether `bake.cmd` is a Bake 0.3 launcher, which names the 0.3 release root
# and starts Node on the TypeScript entry point.
function BakeIsLegacyShim([string]$text) {
  return ($text -match 'BAKE_RELEASE_ROOT' -and $text -match 'apps\\cli\\lib\\bin\.js')
}

# A stalled host fails the install rather than holding it open forever; the
# manifest and its signature are small, so they get a short deadline.
function BakeFetch([string]$url, [string]$path, [int]$timeoutSeconds) {
  # Windows PowerShell 5.1 on .NET before 4.7 defaults to TLS 1.0, which the
  # release host refuses; adding TLS 1.2 leaves whatever else is enabled.
  try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
  } catch { }
  try {
    Invoke-WebRequest $url -UseBasicParsing -TimeoutSec $timeoutSeconds -OutFile $path
  } catch {
    throw "Bake install: could not download $url"
  }
}

function BakeUninstallLinks() {
  $links = Join-Path $nativeRoot 'links.json'
  if (-not (Test-Path -LiteralPath $links -PathType Leaf)) {
    throw "Bake install: $links does not exist, so there is nothing to restore"
  }
  BakeAcquireLock 'install'
  try {
    foreach ($line in [IO.File]::ReadAllLines($links)) {
      if (-not $line.StartsWith('  {"path":"')) { continue }
      $fields = $line -split '"'
      $path = BakeJsonPath $fields[3]
      if ($line -match '"replaced":null') {
        if (Test-Path -LiteralPath $path -PathType Leaf) {
          $text = [IO.File]::ReadAllText($path)
          if ($text -eq (BakeShimText $nativeRoot)) {
            Remove-Item -Force -LiteralPath $path
            Write-Output "Removed $path"
          }
        }
        continue
      }
      if ($line -match '"kind":"file"') {
        $backup = BakeJsonPath $fields[13]
        if (-not (Test-Path -LiteralPath $backup -PathType Leaf)) {
          throw "Bake install: the backup $backup recorded for $path is gone"
        }
        # Only this install's own command is put back: a `bake.cmd` the user
        # has replaced since is left alone, as a created command is.
        if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or ([IO.File]::ReadAllText($path)) -ne (BakeShimText $nativeRoot)) {
          Write-Output "Left $path alone; it is no longer the command this installer made"
          continue
        }
        Remove-Item -Force -LiteralPath $path -ErrorAction SilentlyContinue
        Move-Item -Force -LiteralPath $backup -Destination $path
        Write-Output "Restored $path from $backup"
        continue
      }
      throw "Bake install: $links records a command this installer cannot restore: $line"
    }
    Remove-Item -Force -LiteralPath $links
  } finally {
    BakeReleaseLock
  }
}

# Create `<bin>\bake.cmd`, taking over a Bake-managed 0.3 launcher and
# recording in links.json what was replaced. `dsh.cmd` is never touched.
function BakeLinkCommand([string]$version, [string]$release) {
  New-Item -ItemType Directory -Force -Path $binDir | Out-Null
  $command = Join-Path $binDir 'bake.cmd'
  $shim = BakeShimText $nativeRoot
  $replaced = 'null'
  if (Test-Path -LiteralPath $command) {
    if (-not (Test-Path -LiteralPath $command -PathType Leaf)) {
      throw "Bake install: $command is not a file"
    }
    $existing = [IO.File]::ReadAllText($command)
    if ($existing -eq $shim) {
      # A rerun of this installer, which is how an install is updated. The
      # command already reads `current.txt`, and links.json still records
      # what the first run replaced; backing this shim up and recording it
      # instead would lose the 0.3 command `--uninstall-links` has to
      # restore, and grow <root>\backups on every run.
      Write-Output "Kept $command"
      BakeEnsureOnPath
      return
    }
    $managed = BakeIsLegacyShim $existing
    if (-not $managed -and -not $replaceCommand) {
      Write-Error "Bake install: $command exists and no Bake installer made it, so it was left alone. Bake $version is installed at $release. Run it there, or install again with -ReplaceCommand (BAKE_REPLACE_COMMAND=1) to replace that file and keep a backup." -ErrorAction Continue
      exit 1
    }
    if ($managed) { Write-Output "Taking over $command from the Bake 0.3 install" }
    $backups = Join-Path $nativeRoot 'backups'
    New-Item -ItemType Directory -Force -Path $backups | Out-Null
    $backup = Join-Path $backups ("bake.cmd." + ([DateTimeOffset]::UtcNow).ToUnixTimeSeconds() + ".$PID")
    Move-Item -Force -LiteralPath $command -Destination $backup
    $replaced = '{"kind":"file","backup":"' + (BakeJsonValue $backup) + '"}'
  }
  [IO.File]::WriteAllText($command, $shim, [Text.Encoding]::ASCII)
  $links = Join-Path $nativeRoot 'links.json'
  $linksTemp = "$links.$PID.tmp"
  [IO.File]::WriteAllText($linksTemp, (@(
    '{"schema":"bake-native-links/1","links":[',
    ('  {"path":"' + (BakeJsonValue $command) + '","replaced":' + $replaced + '}'),
    ']}',
    ''
  ) -join "`n"), (New-Object Text.UTF8Encoding $false))
  Move-Item -Force -LiteralPath $linksTemp -Destination $links
  Write-Output "Linked $command"
  BakeEnsureOnPath
}

# Add the command directory to the user PATH, unless it is already there.
function BakeEnsureOnPath() {
  if ($env:BAKE_SKIP_PATH_UPDATE -eq '1') { return }
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  $parts = if ($null -eq $userPath) { @() } else { $userPath -split ';' }
  foreach ($part in $parts) { if ($part.TrimEnd('\') -eq $binDir.TrimEnd('\')) { return } }
  $next = (("$userPath".TrimEnd(';') + ';' + $binDir)).TrimStart(';')
  [Environment]::SetEnvironmentVariable('Path', $next, 'User')
  Write-Output "Added $binDir to your PATH; open a new terminal to use it"
}

function BakeInstall() {
  if (-not [Environment]::Is64BitOperatingSystem) { throw 'Bake install: this Windows platform has no Bake native release archive' }
  if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) { throw 'Bake install: this install needs Windows tar.exe' }
  $target = 'win32-x64'
  $script:temporary = Join-Path ([IO.Path]::GetTempPath()) ('bake-install-' + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $script:temporary | Out-Null

  Write-Output 'Verifying the native release manifest'
  $manifestPath = Join-Path $script:temporary 'stable.json'
  $signaturePath = Join-Path $script:temporary 'stable.json.sig'
  BakeFetch "$downloadUrl/native/stable.json" $manifestPath 60
  BakeFetch "$downloadUrl/native/stable.json.sig" $signaturePath 60
  $key = BakeSpkiPublicKey $nativeReleaseKey
  if ($null -eq $key) { throw 'Bake install: this installer carries no usable native release key' }
  $signatureText = ([IO.File]::ReadAllText($signaturePath)).Trim()
  if ($signatureText -notmatch '^[A-Za-z0-9+/]{86}==$') {
    throw 'Bake install: the native release manifest signature is not a base64 Ed25519 signature'
  }
  $manifestBytes = [IO.File]::ReadAllBytes($manifestPath)
  if (-not (Test-BakeEd25519Signature $key ([Convert]::FromBase64String($signatureText)) $manifestBytes)) {
    throw 'Bake install: the native release manifest signature did not verify'
  }

  # The manifest, as `bake-native-release/1` fixes it, read by the grammar
  # install.sh uses. Whitespace between tokens is removed first; no member of
  # this schema holds any.
  $compact = ([Text.Encoding]::UTF8.GetString($manifestBytes)) -replace '\s', ''
  # The version is canonical SemVer, as every other native reader requires.
  $entry = '"(?:darwin-arm64|darwin-x64|linux-arm64|linux-x64|win32-x64)":\{"file":"[A-Za-z0-9._-]+","sha256":"[0-9a-f]{64}","size":[0-9]+\}'
  $number = '(?:0|[1-9][0-9]*)'
  $identifier = '(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)'
  $grammar = '^\{"schema":"bake-native-release/1","channel":"stable","version":"' + $number + '\.' + $number + '\.' + $number +
    '(?:-' + $identifier + '(?:\.' + $identifier + ')*)?",' +
    '"published":"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z","artifacts":\{' + $entry + '(?:,' + $entry + ')*\}\}$'
  if ($compact -notmatch $grammar) { throw 'Bake install: the native release manifest is not a bake-native-release/1 manifest' }
  foreach ($platform in @('darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64')) {
    if (([regex]::Matches($compact, [regex]::Escape('"' + $platform + '":{'))).Count -gt 1) {
      throw "Bake install: the native release manifest names $platform twice"
    }
  }
  $version = [regex]::Match($compact, '"version":"([^"]+)"').Groups[1].Value
  $found = [regex]::Match($compact, '"' + $target + '":\{"file":"([^"]+)","sha256":"([0-9a-f]{64})","size":([0-9]+)\}')
  if (-not $found.Success) { throw "Bake install: the native release manifest has no $target archive" }
  $file = $found.Groups[1].Value
  $expected = $found.Groups[2].Value
  $size = [long]$found.Groups[3].Value
  if ($file -ne "bake-v$version-$target.tar.gz") {
    throw "Bake install: the manifest names the archive $file, not bake-v$version-$target.tar.gz"
  }
  if ($size -le 0 -or $size -gt 9007199254740991) { throw 'Bake install: the manifest states an impossible archive size' }

  $installed = BakeCurrentVersion
  if ($installed -ne '' -and $installed -ne $version -and -not (BakeVersionNewer $version $installed)) {
    throw "Bake install: the release channel offers Bake $version, older than the installed $installed; this install is unchanged"
  }
  Write-Output "Found Bake $version for $target"

  Write-Output "Downloading $file"
  $archive = Join-Path $script:temporary $file
  BakeFetch "$downloadUrl/native/releases/$version/$file" $archive 1800
  $actualSize = (Get-Item -LiteralPath $archive).Length
  if ($actualSize -ne $size) { throw "Bake install: $file is $actualSize bytes, not the $size the manifest states" }
  $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
  if ($actual -ne $expected) { throw "Bake install: $file did not match the manifest's SHA-256" }
  Write-Output ("Checked SHA-256 " + $expected.Substring(0, 12))

  # Every member, refused before anything is unpacked when it is absolute,
  # holds a `..` component, is anything but a regular file or a directory, or
  # together with the rest unpacks past the bound `bake-update` also applies.
  $maxUnpackedBytes = 536870912
  $listing = & tar.exe -tvzf $archive
  if ($LASTEXITCODE -ne 0) { throw "Bake install: could not read $file" }
  $unpacked = [long]0
  foreach ($line in $listing) {
    if ($line -notmatch '^[-d]') { throw "Bake install: the archive holds a link or special file: $line" }
    if ($line -match ' -> ' -or $line -match ' link to ') { throw "Bake install: the archive holds a link: $line" }
    # The member's size is the field before its date, which BSD tar writes as
    # `Oct 20` and GNU tar as `2026-10-20`.
    $sized = [regex]::Match($line, '\s(\d+)\s+(?:\d{4}-\d{2}-\d{2}|[A-Za-z]{3})\s')
    if ($sized.Success) { $unpacked += [long]$sized.Groups[1].Value }
  }
  if ($unpacked -gt $maxUnpackedBytes) {
    throw "Bake install: the archive unpacks to $unpacked bytes, more than the $maxUnpackedBytes an install accepts"
  }
  foreach ($name in (& tar.exe -tzf $archive)) {
    if ($name -match '^[/\\]' -or $name -match '^[A-Za-z]:' -or $name -match '(^|[/\\])\.\.([/\\]|$)') {
      throw "Bake install: the archive holds the path $name, which leaves the release"
    }
  }

  BakeAcquireLock 'install'
  try {
    $releases = Join-Path $nativeRoot 'releases'
    New-Item -ItemType Directory -Force -Path $releases | Out-Null
    $staging = Join-Path $releases ".staging-$PID"
    Remove-Item -Recurse -Force -LiteralPath $staging -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Path $staging | Out-Null
    & tar.exe -xzf $archive -C $staging
    if ($LASTEXITCODE -ne 0) { throw "Bake install: could not unpack $file" }
    $candidate = BakeReleaseExecutable $staging
    if ($candidate -eq '') { throw 'Bake install: the archive holds no bin\bake executable' }

    Write-Output 'Running the release self-check'
    # System.Diagnostics.Process, not Start-Process: Windows PowerShell 5.1's
    # Start-Process copies the environment into a case-insensitive dictionary
    # and throws when it holds both Path and PATH, as it does when PowerShell
    # starts from Git Bash or MSYS. A ProcessStartInfo whose
    # EnvironmentVariables is never read passes this process's block as is.
    $startInfo = New-Object Diagnostics.ProcessStartInfo
    $startInfo.FileName = $candidate
    $startInfo.Arguments = '--self-check'
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $check = [Diagnostics.Process]::Start($startInfo)
    $reading = $check.StandardOutput.ReadToEndAsync()
    if (-not $check.WaitForExit(60000)) {
      try { $check.Kill() } catch { }
      throw "Bake install: the downloaded Bake $version did not finish its self-check; this install is unchanged"
    }
    if ($check.ExitCode -ne 0) {
      throw "Bake install: the downloaded Bake $version failed its self-check; this install is unchanged"
    }
    if (-not $reading.Wait(10000)) {
      throw "Bake install: the downloaded Bake $version did not finish its self-check; this install is unchanged"
    }
    $record = $reading.Result -replace '\s', ''
    if ($record -notmatch '"kind":"bake-self-check"') {
      throw "Bake install: the downloaded Bake $version printed no self-check record; this install is unchanged"
    }
    if ($record -notmatch ('"version":"' + [regex]::Escape($version) + '"')) {
      throw "Bake install: the downloaded archive does not report version $version; this install is unchanged"
    }
    if ($record -notmatch ('"target":"' + $target + '"')) {
      throw "Bake install: the downloaded archive does not report platform $target; this install is unchanged"
    }

    $destination = Join-Path $releases $version
    if (Test-Path -LiteralPath $destination) {
      $replacedRelease = Join-Path $releases ".replaced-$PID"
      Remove-Item -Recurse -Force -LiteralPath $replacedRelease -ErrorAction SilentlyContinue
      Move-Item -LiteralPath $destination -Destination $replacedRelease
      Remove-Item -Recurse -Force -LiteralPath $replacedRelease -ErrorAction SilentlyContinue
    }
    Move-Item -LiteralPath $staging -Destination $destination

    $previous = BakeCurrentVersion
    if ($previous -ne '' -and $previous -ne $version) { BakeWritePointer 'previous.txt' $previous }
    BakeWritePointer 'current.txt' $version
    Write-Output "Installed Bake $version at $destination"

    # Retention: current, previous, and nothing else. A release whose files
    # are held, such as a running executable, is left for a later prune.
    $keep = @($version)
    $previousPointer = Join-Path $nativeRoot 'previous.txt'
    if (Test-Path -LiteralPath $previousPointer -PathType Leaf) { $keep += ([IO.File]::ReadAllText($previousPointer)).Trim() }
    foreach ($release in (Get-ChildItem -LiteralPath $releases -Directory)) {
      if ($keep -contains $release.Name) { continue }
      Remove-Item -Recurse -Force -LiteralPath $release.FullName -ErrorAction SilentlyContinue
    }

    BakeLinkCommand $version $destination
  } finally {
    BakeReleaseLock
  }
}

try {
  if ($uninstallLinks) {
    BakeUninstallLinks
  } else {
    BakeInstall
    Write-Output 'Run: bake'
  }
} finally {
  BakeReleaseLock
  if ($null -ne $temporary) { Remove-Item -Recurse -Force -LiteralPath $temporary -ErrorAction SilentlyContinue }
}
