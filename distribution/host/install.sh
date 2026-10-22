#!/bin/sh
# The Bake native installer: it installs the Rust agent, `bake`, from the
# signed native release channel. Bake 0.4.0 publishes Linux x64 and arm64
# releases only; on macOS an install stops before it downloads anything, since
# those releases arrive in 0.4.x. POSIX sh only, and the host needs no Node or
# Bun: it needs curl or wget, tar, and sha256sum or shasum. The manifest
# signature is verified by an OpenSSL with Ed25519 (1.1.1 or newer) where the
# host has one, and otherwise, where `osascript` exists, by the Ed25519
# verifier this script embeds, run by `osascript -l JavaScript`; the macOS
# releases of 0.4.x rely on it.
#
#   curl -fsSL https://bake.justar.dev/install.sh | sh
#
# See distribution/README.md, "Native installers", for the route, the install
# layout, the `bake` takeover, and the lock these steps take.
set -eu

download_url=${BAKE_DOWNLOAD_URL:-https://bake.justar.dev}
download_url=${download_url%/}
native_root=${BAKE_NATIVE_ROOT:-"$HOME/.local/share/bake-native"}
bin_dir=${BAKE_BIN_DIR:-"$HOME/.local/bin"}
# The 0.3 TypeScript install, which this installer reads to recognize a
# Bake-managed `bake` and never writes to.
legacy_root=${BAKE_INSTALL_ROOT:-"$HOME/.local/share/bake"}
replace_command=${BAKE_REPLACE_COMMAND:-}
# The native release key (distribution/host/native-release-key.pub), base64
# DER SubjectPublicKeyInfo. A release test holds every copy equal. Tests
# replace this one line in a private copy of the installer, so the published
# installer takes no key from its environment.
native_release_key='MCowBQYDK2VwAyEAYHdFEaVkRABjXEFzF1WnfoX1MJUcwzgB9oS9kyzF9hY='

mode=install
while [ $# -gt 0 ]; do
  case $1 in
    --replace-command) replace_command=1 ;;
    --uninstall-links) mode=uninstall-links ;;
    --help|-h)
      cat <<'USAGE'
Install Bake, the Rust terminal coding agent.

  --replace-command   replace a `bake` command this installer did not make,
                      keeping a backup it can restore
  --uninstall-links   restore the commands recorded in <root>/links.json and
                      remove the links this installer made

Environment:
  BAKE_DOWNLOAD_URL   release host (default https://bake.justar.dev)
  BAKE_NATIVE_ROOT    install root (default ~/.local/share/bake-native)
  BAKE_BIN_DIR        command directory (default ~/.local/bin)
  BAKE_REPLACE_COMMAND=1  as --replace-command, for a piped install
  BAKE_OPENSSL        the openssl that verifies the release signature; when
                      it is set, no other openssl is tried
USAGE
      exit 0 ;;
    *) printf 'Bake install: unknown option %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

fail() {
  printf 'Bake install: %s\n' "$*" >&2
  exit 1
}

step() {
  printf '%s\n' "$*"
}

need() {
  command -v "$1" >/dev/null 2>&1
}

temporary=
lock_dir="$native_root/install.lock"
lock_held=
release_lock() {
  if [ -n "$lock_held" ]; then
    lock_held=
    rm -f "$lock_dir/owner"
    rmdir "$lock_dir" 2>/dev/null || rm -rf "$lock_dir" 2>/dev/null || :
  fi
}
cleanup() {
  release_lock
  [ -z "$temporary" ] || rm -rf "$temporary"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# `<root>/install.lock` is a directory, created atomically by mkdir, holding
# one `owner` line of `<pid> <unix seconds> <kind>`. The Rust updater takes
# the same lock, so both sides must agree on this rule: a held lock whose
# owner process is gone is stale and is taken over once; a lock with no owner
# file counts as held for ten more seconds, then as stale.
acquire_lock() {
  kind=$1
  mkdir -p "$native_root"
  attempt=0
  while :; do
    if mkdir "$lock_dir" 2>/dev/null; then
      lock_held=1
      printf '%s %s %s\n' "$$" "$(date +%s)" "$kind" > "$lock_dir/owner"
      return 0
    fi
    [ "$attempt" -eq 0 ] || fail "another Bake install holds $lock_dir; try again when it finishes"
    attempt=1
    lock_stale || fail "another Bake install holds $lock_dir; try again when it finishes"
    rm -f "$lock_dir/owner"
    rmdir "$lock_dir" 2>/dev/null || rm -rf "$lock_dir" 2>/dev/null || :
  done
}

# Whether the held lock may be taken over. A lock with no owner file is
# waited out rather than read from the directory's timestamp, which no
# portable sh reads; waiting only ever treats a lock as held for longer.
lock_stale() {
  waited=0
  while [ ! -f "$lock_dir/owner" ]; do
    [ "$waited" -lt 10 ] || return 0
    sleep 1
    waited=$((waited + 1))
    # The holder released the lock while we waited: the caller's next mkdir
    # takes it, as the Rust updater's own retry does.
    [ -d "$lock_dir" ] || return 0
  done
  owner_pid=$(awk 'NR == 1 { print $1 }' "$lock_dir/owner" 2>/dev/null || :)
  case $owner_pid in
    '' | *[!0-9]*) return 0 ;;
  esac
  kill -0 "$owner_pid" 2>/dev/null && return 1
  return 0
}

# A stalled host fails the install rather than holding it open forever: the
# third argument is how many seconds the whole transfer may take.
fetch_to() {
  if need curl; then
    curl -fsSL --connect-timeout 20 --max-time "$3" "$1" -o "$2" || fail "could not download $1"
  else
    wget -q --timeout=20 --tries=2 -O "$2" "$1" || fail "could not download $1"
  fi
}

sha256_of() {
  if need sha256sum; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

sha512_of() {
  if need shasum; then
    shasum -a 512 "$1" | cut -d ' ' -f 1
  elif need sha512sum; then
    sha512sum "$1" | cut -d ' ' -f 1
  else
    return 1
  fi
}

# A file's bytes as lowercase hex, which `od` prints on every POSIX host.
hex_of() {
  od -An -v -tx1 < "$1" | tr -d ' \n'
}

# Decode base64 into a file. `-d` is GNU and current macOS; `-D` is the older
# BSD spelling the Macs that need the embedded verifier may still have.
base64_decode() {
  base64 -d < "$1" > "$2" 2>/dev/null || base64 -D < "$1" > "$2" 2>/dev/null
}

byte_count() {
  wc -c < "$1" | tr -d ' '
}

# A path as a JSON string's content for links.json. Every backslash and quote
# becomes a `\uXXXX` escape, so an escaped value holds no raw quote or
# backslash and the reader can split a record on quotes.
json_value() {
  printf '%s' "$1" | sed 's/\\/\\u005c/g; s/"/\\u0022/g'
}

json_path() {
  printf '%s' "$1" | sed 's/\\u0022/"/g; s/\\u005c/\\/g'
}

# BEGIN VERSION ORDER
# Whether release `$1` is newer than `$2`, by the SemVer precedence
# bake-update's `compare_versions` applies: a pre-release is older than its
# own release; identifiers compare one by one, numeric ones by value at any
# length, alphanumeric ones as ASCII text, a numeric one before an
# alphanumeric one, and a shorter list first when the other extends it.
# Numbers are compared as digit strings, never as awk numbers, which lose
# precision past 2^53; the C locale keeps text comparison bytewise.
# scripts/release/fixtures/native-version-order.txt holds the cases every
# implementation is tested against.
version_newer() {
  LC_ALL=C awk -v left="$1" -v right="$2" '
    function digits(x) { return x ~ /^[0-9]+$/ }
    function text(x, y) { x = x ""; y = y ""; return x == y ? 0 : (x < y ? -1 : 1) }
    function number(x, y) {
      sub(/^0+/, "", x); sub(/^0+/, "", y)
      if (length(x) != length(y)) return length(x) < length(y) ? -1 : 1
      return text(x, y)
    }
    function identifier(x, y) {
      if (digits(x) && digits(y)) return number(x, y)
      if (digits(x)) return -1
      if (digits(y)) return 1
      return text(x, y)
    }
    function compare(l, r,    lc, rc, lp, rp, a, b, n, m, i, o) {
      lc = l; lp = ""; if (index(l, "-")) { lc = substr(l, 1, index(l, "-") - 1); lp = substr(l, index(l, "-") + 1) }
      rc = r; rp = ""; if (index(r, "-")) { rc = substr(r, 1, index(r, "-") - 1); rp = substr(r, index(r, "-") + 1) }
      split(lc, a, "."); split(rc, b, ".")
      for (i = 1; i <= 3; i++) {
        o = number(digits(a[i]) ? a[i] : "0", digits(b[i]) ? b[i] : "0")
        if (o != 0) return o
      }
      if (!index(l, "-") && !index(r, "-")) return 0
      if (!index(l, "-")) return 1
      if (!index(r, "-")) return -1
      n = split(lp, a, "."); m = split(rp, b, ".")
      for (i = 1; i <= n && i <= m; i++) {
        o = identifier(a[i], b[i])
        if (o != 0) return o
      }
      return n == m ? 0 : (n < m ? -1 : 1)
    }
    BEGIN { print (compare(left, right) > 0) ? "yes" : "no" }
  ' | grep -q '^yes$'
}
# END VERSION ORDER

# RFC 8032 section 7.1 TEST 2: the public key as the installers carry one,
# base64 DER SubjectPublicKeyInfo, and its signature over the one-byte
# message `r` (0x72). A verifier must accept this pair over that message and
# refuse it over another, which is how this script learns that a candidate
# really verifies Ed25519 rather than reporting support it does not have.
# The message is not empty: OpenSSL 3.5's pkeyutl refuses a zero-byte input.
ed25519_test_key='MCowBQYDK2VwAyEAPUAXw+hDiVqStwqnTRt+vJyYLM8uxJaMwM1V8Sr0Zgw='
ed25519_test_signature='kqAJqfDUyrhyDoILX2QlQKKye1QWUD+Ps3YiI+vbadoIWsHkPhWZbkWPNhPQ8R2MOHsurrQwKu6wDSkWErsMAA=='

# How the manifest signature is verified: `openssl`, with the proven command
# in openssl_command, or `javascript`, the verifier this script embeds.
verifier=
openssl_command=

# Whether `$2`, base64, is a valid Ed25519 signature by the key `$1`, base64
# DER SubjectPublicKeyInfo, over the exact bytes of the file `$3`.
verify_signature() {
  if [ "$verifier" = javascript ]; then
    javascript_verify "$1" "$2" "$3"
  else
    openssl_verify "$1" "$2" "$3"
  fi
}

openssl_verify() {
  work="$temporary/verify"
  rm -rf "$work"
  mkdir -p "$work"
  {
    printf '%s\n' '-----BEGIN PUBLIC KEY-----'
    printf '%s\n' "$1" | fold -w 64
    printf '%s\n' '-----END PUBLIC KEY-----'
  } > "$work/key.pem"
  printf '%s' "$2" > "$work/signature.base64"
  "$openssl_command" base64 -d -A -in "$work/signature.base64" -out "$work/signature.bin" 2>/dev/null || return 1
  "$openssl_command" pkeyutl -verify -pubin -inkey "$work/key.pem" -rawin \
    -in "$3" -sigfile "$work/signature.bin" >/dev/null 2>&1
}

# The embedded verifier: this script assembles R || A || M and its SHA-512,
# and the JavaScript does the point decompression and the group check.
javascript_verify() {
  work="$temporary/verify"
  rm -rf "$work"
  mkdir -p "$work"
  printf '%s' "$1" > "$work/key.base64"
  base64_decode "$work/key.base64" "$work/key.der" || return 1
  [ "$(byte_count "$work/key.der")" = 44 ] || return 1
  # The fixed Ed25519 SubjectPublicKeyInfo header, then the 32 key bytes.
  case $(hex_of "$work/key.der") in
    302a300506032b6570032100*) ;;
    *) return 1 ;;
  esac
  tail -c 32 "$work/key.der" > "$work/key.bin"
  printf '%s' "$2" > "$work/signature.base64"
  base64_decode "$work/signature.base64" "$work/signature.bin" || return 1
  [ "$(byte_count "$work/signature.bin")" = 64 ] || return 1
  head -c 32 "$work/signature.bin" > "$work/commitment.bin"
  cat "$work/commitment.bin" "$work/key.bin" "$3" > "$work/signed.bin"
  digest=$(sha512_of "$work/signed.bin") || return 1
  ed25519_javascript > "$work/ed25519.js"
  answer=$(osascript -l JavaScript "$work/ed25519.js" \
    "$(hex_of "$work/key.bin")" "$(hex_of "$work/signature.bin")" "$digest" 2>/dev/null || :)
  [ "$answer" = true ]
}

# BEGIN ED25519 JAVASCRIPT VERIFIER
# macOS ships LibreSSL, whose openssl(1) has neither `pkeyutl -rawin` nor
# Ed25519, so a stock Mac has no OpenSSL this script can verify with and runs
# no Node. `osascript -l JavaScript` is JavaScriptCore, which has BigInt on
# every supported macOS, so the verifier below is the installer's own code.
# The tests extract this heredoc and run its exact text under Bun and Node.
ed25519_javascript() {
  cat <<'BAKE_ED25519_JAVASCRIPT'
'use strict'
// Ed25519 verification, RFC 8032 section 5.1.7, over hex arguments: the
// 32-byte public key, the 64-byte signature, and the SHA-512 of R || A || M,
// which the caller computes with `shasum -a 512`. Only public keys,
// signatures, and release bytes pass through here, so no secret is handled
// and no timing side channel matters.
var BAKE_P = (1n << 255n) - 19n
var BAKE_L = (1n << 252n) + 27742317777372353535851937790883648493n

function bakeFieldMod(value) {
  var residue = value % BAKE_P
  return residue < 0n ? residue + BAKE_P : residue
}

function bakePower(base, exponent) {
  var result = 1n
  var factor = bakeFieldMod(base)
  var remaining = exponent
  while (remaining > 0n) {
    if (remaining & 1n) result = bakeFieldMod(result * factor)
    factor = bakeFieldMod(factor * factor)
    remaining >>= 1n
  }
  return result
}

// The curve constant d = -121665/121666 and the square root of -1.
var BAKE_D = bakeFieldMod(-121665n * bakePower(121666n, BAKE_P - 2n))
var BAKE_SQRT_MINUS_ONE = bakePower(2n, (BAKE_P - 1n) / 4n)

// Hex as bytes, or null when it is not an even run of hex digits.
function bakeHexBytes(hex) {
  if (typeof hex !== 'string' || hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) return null
  var bytes = []
  for (var index = 0; index < hex.length; index += 2) bytes.push(parseInt(hex.substr(index, 2), 16))
  return bytes
}

// A little-endian byte string as a non-negative integer.
function bakeLittleEndian(bytes) {
  var value = 0n
  for (var index = bytes.length - 1; index >= 0; index--) value = (value << 8n) | BigInt(bytes[index])
  return value
}

// Points are extended twisted Edwards coordinates (X, Y, Z, T), where this
// addition is complete for the curve, so it doubles as well.
function bakePointAdd(left, right) {
  var a = bakeFieldMod((left[1] - left[0]) * (right[1] - right[0]))
  var b = bakeFieldMod((left[1] + left[0]) * (right[1] + right[0]))
  var c = bakeFieldMod(left[3] * 2n * BAKE_D * right[3])
  var d = bakeFieldMod(left[2] * 2n * right[2])
  var e = b - a
  var f = d - c
  var g = d + c
  var h = b + a
  return [bakeFieldMod(e * f), bakeFieldMod(g * h), bakeFieldMod(f * g), bakeFieldMod(e * h)]
}

function bakeScalarMultiply(point, scalar) {
  var result = [0n, 1n, 1n, 0n]
  var addend = point
  var remaining = scalar
  while (remaining > 0n) {
    if (remaining & 1n) result = bakePointAdd(result, addend)
    addend = bakePointAdd(addend, addend)
    remaining >>= 1n
  }
  return result
}

function bakePointEquals(left, right) {
  if (bakeFieldMod(left[0] * right[2]) !== bakeFieldMod(right[0] * left[2])) return false
  return bakeFieldMod(left[1] * right[2]) === bakeFieldMod(right[1] * left[2])
}

// RFC 8032 section 5.1.3: 32 little-endian bytes as a point, or null for a
// non-canonical encoding (y at or above p), a y with no x, or x = 0 with the
// sign bit set.
function bakePointDecode(bytes) {
  if (bytes === null || bytes.length !== 32) return null
  var sign = (bytes[31] >> 7) & 1
  var copy = bytes.slice(0, 32)
  copy[31] = copy[31] & 0x7f
  var y = bakeLittleEndian(copy)
  if (y >= BAKE_P) return null
  var square = bakeFieldMod(y * y)
  var u = bakeFieldMod(square - 1n)
  var v = bakeFieldMod(BAKE_D * square + 1n)
  var candidate = bakeFieldMod(u * bakePower(v, 3n) * bakePower(bakeFieldMod(u * bakePower(v, 7n)), (BAKE_P - 5n) / 8n))
  var check = bakeFieldMod(v * candidate * candidate)
  if (check !== u) {
    if (check !== bakeFieldMod(-u)) return null
    candidate = bakeFieldMod(candidate * BAKE_SQRT_MINUS_ONE)
  }
  if (candidate === 0n && sign === 1) return null
  if (Number(candidate % 2n) !== sign) candidate = BAKE_P - candidate
  return [candidate, y, 1n, bakeFieldMod(candidate * y)]
}

var BAKE_BASE = bakePointDecode(bakeHexBytes('5866666666666666666666666666666666666666666666666666666666666666'))

// The cofactorless check [S]B = R + [k]A. A signature whose S is at or above
// the group order, whose R or A is non-canonical or undecodable, or whose
// lengths differ from 32, 64, and 64 bytes is refused before any curve
// arithmetic.
function bakeEd25519Verify(keyHex, signatureHex, digestHex) {
  var key = bakeHexBytes(keyHex)
  var signature = bakeHexBytes(signatureHex)
  var digest = bakeHexBytes(digestHex)
  if (key === null || key.length !== 32) return false
  if (signature === null || signature.length !== 64) return false
  if (digest === null || digest.length !== 64) return false
  var s = bakeLittleEndian(signature.slice(32, 64))
  if (s >= BAKE_L) return false
  var publicPoint = bakePointDecode(key)
  if (publicPoint === null) return false
  var commitment = bakePointDecode(signature.slice(0, 32))
  if (commitment === null) return false
  var k = bakeLittleEndian(digest) % BAKE_L
  var left = bakeScalarMultiply(BAKE_BASE, s)
  var right = bakePointAdd(commitment, bakeScalarMultiply(publicPoint, k))
  return bakePointEquals(left, right)
}

// osascript passes the script's arguments to `run` and prints what it returns.
function run(argv) {
  return bakeEd25519Verify(argv[0], argv[1], argv[2]) ? 'true' : 'false'
}
BAKE_ED25519_JAVASCRIPT
}
# END ED25519 JAVASCRIPT VERIFIER

# Choose how this host verifies the release signature, and prove the choice
# before anything is downloaded: the candidate must accept RFC 8032's TEST 1
# signature over its own message and refuse it over another one.
set_verifier() {
  probe="$temporary/probe"
  rm -rf "$probe"
  mkdir -p "$probe"
  printf 'r' > "$probe/message"
  printf 'x' > "$probe/other"
  tried=
  if [ -n "${BAKE_OPENSSL:-}" ]; then
    # The one this host was told to use, and no other.
    probe_openssl "$BAKE_OPENSSL" && return 0
  else
    probe_openssl openssl && return 0
    probe_openssl /opt/homebrew/opt/openssl@3/bin/openssl && return 0
    probe_openssl /usr/local/opt/openssl@3/bin/openssl && return 0
  fi
  # Only macOS has osascript, so this is the Mac's fallback; it is proven on
  # the same vector, before the manifest is read.
  if need osascript && need base64 && need od && { need shasum || need sha512sum; }; then
    verifier=javascript
    if probe_verifier; then
      return 0
    fi
    verifier=
    tried="$tried
  the verifier this script embeds, run by osascript"
  fi
  fail "nothing here verifies the release signature, which is Ed25519. Tried:$tried
Install OpenSSL 3 (brew install openssl@3) or name one with BAKE_OPENSSL."
}

# Whether the openssl at `$1` exists and verifies Ed25519, which makes it
# this install's verifier.
probe_openssl() {
  if ! need "$1"; then
    tried="$tried
  $1, which is not installed"
    return 1
  fi
  verifier=openssl
  openssl_command=$1
  if probe_verifier; then
    return 0
  fi
  verifier=
  openssl_command=
  tried="$tried
  $1"
  return 1
}

# Whether the chosen verifier accepts the embedded vector over its own
# message and refuses it over another one.
probe_verifier() {
  verify_signature "$ed25519_test_key" "$ed25519_test_signature" "$probe/message" \
    && ! verify_signature "$ed25519_test_key" "$ed25519_test_signature" "$probe/other"
}

# Set `target` to this host's release platform, or stop on a platform Bake
# 0.4.0 does not release for.
set_platform() {
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target=darwin-arm64 ;;
    Darwin-x86_64) target=darwin-x64 ;;
    Linux-aarch64 | Linux-arm64) target=linux-arm64 ;;
    Linux-x86_64) target=linux-x64 ;;
    *) fail "this platform has no Bake native release archive" ;;
  esac
  # BEGIN 0.4.0 PLATFORM REFUSAL
  # Bake 0.4.0 releases Linux alone (D47); the macOS archives arrive in 0.4.x.
  # The tests run the install path on macOS through a copy without this block.
  case $target in
    linux-*) ;;
    *) fail "Bake 0.4.0 runs on Linux only; macOS support is coming in a later 0.4 release." ;;
  esac
  # END 0.4.0 PLATFORM REFUSAL
}

# The release `current` names, or empty when the install has no pointer yet.
pointer_version() {
  pointer=$1
  [ -L "$pointer" ] || return 0
  target=$(readlink "$pointer") || return 0
  case $target in
    releases/*) printf '%s' "${target#releases/}" ;;
    */releases/*) printf '%s' "${target##*/releases/}" ;;
  esac
}

# `mv` dereferences a destination that is a symbolic link to a directory,
# which `current` is, and no POSIX option says otherwise: GNU spells it `-T`
# and BSD, including macOS, `-h`. The one this host has is found once, so a
# pointer is always replaced by a rename rather than by removing it first.
pointer_mv=
set_pointer_mv() {
  probe="$temporary/mv-probe"
  for flags in -fT -fh; do
    rm -rf "$probe"
    mkdir -p "$probe/dir"
    : > "$probe/file"
    ln -s dir "$probe/link"
    if mv $flags "$probe/file" "$probe/link" 2>/dev/null && [ ! -L "$probe/link" ] && [ -f "$probe/link" ]; then
      pointer_mv=$flags
      rm -rf "$probe"
      return 0
    fi
  done
  rm -rf "$probe"
  fail 'this mv replaces no symbolic link atomically; it needs mv -T (GNU) or mv -h (BSD)'
}

# Replace `$1` with a relative symbolic link to `$2` under the root, through a
# temporary link and a rename, so a reader sees one target or the other.
point_at() {
  pointer=$1
  pointer_temp="$native_root/.$(basename "$pointer").$$"
  rm -f "$pointer_temp"
  ln -s "releases/$2" "$pointer_temp"
  mv $pointer_mv "$pointer_temp" "$pointer"
}

# The executable inside an unpacked release, which the archive holds as
# `bin/bake`.
release_executable() {
  if [ -f "$1/bin/bake" ]; then
    printf '%s' "$1/bin/bake"
  fi
}

# Run the candidate's own `--self-check` within a deadline and require the
# version and platform the manifest promised. The pointer never moves without
# this.
self_check() {
  candidate=$1
  want_version=$2
  want_target=$3
  "$candidate" --self-check > "$temporary/self-check.out" 2> "$temporary/self-check.err" &
  check_pid=$!
  waited=0
  while kill -0 "$check_pid" 2>/dev/null; do
    if [ "$waited" -ge 60 ]; then
      kill -9 "$check_pid" 2>/dev/null || :
      wait "$check_pid" 2>/dev/null || :
      fail "the downloaded Bake $want_version did not finish its self-check; this install is unchanged"
    fi
    sleep 1
    waited=$((waited + 1))
  done
  wait "$check_pid" || fail "the downloaded Bake $want_version failed its self-check; this install is unchanged"
  record=$(tr -d '[:space:]' < "$temporary/self-check.out")
  case $record in
    *'"kind":"bake-self-check"'*) ;;
    *) fail "the downloaded Bake $want_version printed no self-check record; this install is unchanged" ;;
  esac
  case $record in
    *'"version":"'"$want_version"'"'*) ;;
    *) fail "the downloaded archive does not report version $want_version; this install is unchanged" ;;
  esac
  case $record in
    *'"target":"'"$want_target"'"'*) ;;
    *) fail "the downloaded archive does not report platform $want_target; this install is unchanged" ;;
  esac
}

# The most a release archive may unpack to, as `bake-update`'s archive reader
# bounds it: a native release is tens of megabytes, and this stops a
# decompression bomb long before the disk fills.
max_unpacked_bytes=536870912

# Every member of the archive, refused before anything is unpacked when it is
# absolute, holds a `..` component, is anything but a regular file or a
# directory, or together with the rest unpacks past max_unpacked_bytes.
check_members() {
  archive=$1
  tar -tzf "$archive" > "$temporary/members" 2>/dev/null || fail "could not read $archive"
  [ -s "$temporary/members" ] || fail "$archive holds no files"
  while IFS= read -r name; do
    case $name in
      /* | ~*) fail "the archive holds the absolute path $name" ;;
      .. | ../* | */../* | */..) fail "the archive holds the path $name, which leaves the release" ;;
    esac
  done < "$temporary/members"
  tar -tvzf "$archive" > "$temporary/members-long" 2>/dev/null || fail "could not read $archive"
  while IFS= read -r line; do
    case $line in
      -* | d*) ;;
      *) fail "the archive holds a link or special file: $line" ;;
    esac
    case $line in
      *' -> '* | *' link to '*) fail "the archive holds a link: $line" ;;
    esac
  done < "$temporary/members-long"
  # Each listed member's size is the field before its date, which GNU tar
  # writes as `2026-10-20` and BSD tar as `Oct 20`.
  unpacked=$(awk '
    {
      for (i = 2; i < NF; i++) {
        if ($i ~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]$/ || $i ~ /^[A-Z][a-z][a-z]$/) {
          if ($(i - 1) ~ /^[0-9]+$/) total += $(i - 1)
          break
        }
      }
    }
    END { printf "%.0f", total }
  ' "$temporary/members-long")
  [ "$unpacked" -le "$max_unpacked_bytes" ] \
    || fail "the archive unpacks to $unpacked bytes, more than the $max_unpacked_bytes an install accepts"
}

# Where `<bin>/bake` points, as far as the takeover is concerned:
# `none`, `native`, `legacy` (a Bake-managed 0.3 link), or `foreign`.
command_owner() {
  path=$1
  if [ -L "$path" ]; then
    target=$(readlink "$path")
    case $target in
      /*) ;;
      *) target="$(dirname "$path")/$target" ;;
    esac
    case $target in
      "$native_root"/*) printf 'native' ;;
      "$legacy_root"/*) printf 'legacy' ;;
      *) printf 'foreign' ;;
    esac
    return 0
  fi
  if [ -e "$path" ]; then
    printf 'foreign'
    return 0
  fi
  printf 'none'
}

uninstall_links() {
  links="$native_root/links.json"
  [ -f "$links" ] || fail "$links does not exist, so there is nothing to restore"
  acquire_lock install
  restored=0
  while IFS= read -r line; do
    case $line in
      '  {"path":"'*) ;;
      *) continue ;;
    esac
    path=$(json_path "$(printf '%s' "$line" | cut -d '"' -f 4)")
    case $line in
      *'"replaced":null'*)
        if [ "$(command_owner "$path")" = native ]; then
          rm -f "$path"
          step "Removed $path"
          restored=$((restored + 1))
        fi
        ;;
      *'"kind":"symlink"'*)
        target=$(json_path "$(printf '%s' "$line" | cut -d '"' -f 14)")
        # Only this install's own command is put back, as for a command it
        # created: a `bake` the user has replaced since is left alone.
        if [ "$(command_owner "$path")" = native ]; then
          rm -f "$path"
          ln -s "$target" "$path"
          step "Restored $path -> $target"
          restored=$((restored + 1))
        else
          step "Left $path alone; it is no longer the command this installer made"
        fi
        ;;
      *'"kind":"file"'*)
        backup=$(json_path "$(printf '%s' "$line" | cut -d '"' -f 14)")
        [ -e "$backup" ] || fail "the backup $backup recorded for $path is gone"
        if [ "$(command_owner "$path")" = native ]; then
          rm -f "$path"
          mv -f "$backup" "$path"
          step "Restored $path from $backup"
          restored=$((restored + 1))
        else
          step "Left $path alone; it is no longer the command this installer made"
        fi
        ;;
      *) fail "$links records a command this installer cannot restore: $line" ;;
    esac
  done < "$links"
  rm -f "$links"
  release_lock
  step "Restored $restored command(s); the releases under $native_root were left in place."
}

install_release() {
  set_platform
  temporary=$(mktemp -d "${TMPDIR:-/tmp}/bake-install.XXXXXX")
  set_pointer_mv
  set_verifier

  if [ "$verifier" = javascript ]; then
    step 'Verifying the native release manifest with the embedded JavaScript verifier (osascript)'
  else
    step "Verifying the native release manifest with $openssl_command"
  fi
  fetch_to "$download_url/native/stable.json" "$temporary/stable.json" 60
  fetch_to "$download_url/native/stable.json.sig" "$temporary/stable.json.sig" 60
  tr -d '[:space:]' < "$temporary/stable.json.sig" > "$temporary/signature.base64"
  # A base64 decoder rejects nothing outside base64, so the text is checked
  # first and the signature must be Ed25519's 64 bytes.
  grep -q '^[A-Za-z0-9+/]\{86\}==$' "$temporary/signature.base64" \
    || fail 'the native release manifest signature is not a base64 Ed25519 signature'
  verify_signature "$native_release_key" "$(cat "$temporary/signature.base64")" "$temporary/stable.json" \
    || fail 'the native release manifest signature did not verify'

  # The manifest, as `bake-native-release/1` fixes it. Whitespace between
  # tokens is removed first; no member of this schema holds any.
  tr -d '[:space:]' < "$temporary/stable.json" > "$temporary/manifest"
  # The grammar names all five platforms, so a 0.4.x manifest still reads;
  # the version is canonical SemVer, as every other native reader requires.
  entry='"(darwin-arm64|darwin-x64|linux-arm64|linux-x64|win32-x64)":\{"file":"[A-Za-z0-9._-]+","sha256":"[0-9a-f]{64}","size":[0-9]+\}'
  number='(0|[1-9][0-9]*)'
  identifier='(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)'
  grep -Eq '^\{"schema":"bake-native-release/1","channel":"stable","version":"'"$number"'\.'"$number"'\.'"$number"'(-'"$identifier"'(\.'"$identifier"')*)?","published":"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z","artifacts":\{'"$entry"'(,'"$entry"')*\}\}$' \
    "$temporary/manifest" || fail 'the native release manifest is not a bake-native-release/1 manifest'
  repeated=$(awk -v names='darwin-arm64 darwin-x64 linux-arm64 linux-x64 win32-x64' '
    {
      split(names, platforms, " ")
      for (i = 1; i <= 5; i++) {
        rest = $0; key = "\"" platforms[i] "\":{"; seen = 0
        while ((at = index(rest, key)) > 0) { seen++; rest = substr(rest, at + length(key)) }
        if (seen > 1) { print platforms[i]; exit }
      }
    }
  ' "$temporary/manifest")
  [ -z "$repeated" ] || fail "the native release manifest names $repeated twice"
  version=$(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' "$temporary/manifest")
  artifact=$(sed -n 's/.*"'"$target"'":{\("file":"[^"]*","sha256":"[0-9a-f]*","size":[0-9]*\)}.*/\1/p' "$temporary/manifest")
  [ -n "$artifact" ] || fail "the native release manifest has no $target archive"
  file=$(printf '%s' "$artifact" | sed -n 's/.*"file":"\([^"]*\)".*/\1/p')
  expected=$(printf '%s' "$artifact" | sed -n 's/.*"sha256":"\([^"]*\)".*/\1/p')
  size=$(printf '%s' "$artifact" | sed -n 's/.*"size":\([0-9]*\).*/\1/p')
  [ "$file" = "bake-v$version-$target.tar.gz" ] || fail "the manifest names the archive $file, not bake-v$version-$target.tar.gz"
  case $size in
    0* | '') fail 'the manifest states no archive size' ;;
  esac
  [ "$size" -le 9007199254740991 ] || fail 'the manifest states an impossible archive size'

  installed=$(pointer_version "$native_root/current")
  if [ -n "$installed" ] && [ "$installed" != "$version" ] && ! version_newer "$version" "$installed"; then
    fail "the release channel offers Bake $version, older than the installed $installed; this install is unchanged"
  fi
  step "Found Bake $version for $target"

  step "Downloading $file"
  fetch_to "$download_url/native/releases/$version/$file" "$temporary/$file" 1800
  actual_size=$(wc -c < "$temporary/$file" | tr -d ' ')
  [ "$actual_size" = "$size" ] || fail "$file is $actual_size bytes, not the $size the manifest states"
  actual=$(sha256_of "$temporary/$file")
  [ "$actual" = "$expected" ] || fail "$file did not match the manifest's SHA-256"
  step "Checked SHA-256 $(printf '%s' "$expected" | cut -c 1-12)"

  check_members "$temporary/$file"

  acquire_lock install
  mkdir -p "$native_root/releases" "$bin_dir"
  sweep_leftovers
  staging="$native_root/releases/.staging-$$"
  rm -rf "$staging"
  mkdir "$staging"
  tar -xzf "$temporary/$file" -C "$staging" || fail "could not unpack $file"
  if [ -n "$(find "$staging" ! -type d ! -type f -print)" ]; then
    fail "the unpacked release holds a link or special file"
  fi
  candidate=$(release_executable "$staging")
  [ -n "$candidate" ] || fail "the archive holds no bin/bake executable"
  executable_name=$(basename "$candidate")

  step 'Running the release self-check'
  self_check "$candidate" "$version" "$target"

  destination="$native_root/releases/$version"
  replaced_release="$native_root/releases/.replaced-$$"
  if [ -e "$destination" ]; then
    rm -rf "$replaced_release"
    mv "$destination" "$replaced_release"
  fi
  mv "$staging" "$destination"
  rm -rf "$replaced_release" 2>/dev/null || :

  previous=$(pointer_version "$native_root/current")
  if [ -n "$previous" ] && [ "$previous" != "$version" ]; then
    point_at "$native_root/previous" "$previous"
  fi
  point_at "$native_root/current" "$version"
  step "Installed Bake $version at $destination"

  # Retention: current, previous, and nothing else. A release whose files are
  # held is left for a later prune rather than failing the install.
  keep_previous=$(pointer_version "$native_root/previous")
  for release in "$native_root/releases"/*; do
    [ -d "$release" ] || continue
    name=$(basename "$release")
    [ "$name" != "$version" ] || continue
    [ "$name" != "$keep_previous" ] || continue
    rm -rf "$release" 2>/dev/null || :
  done

  link_command
  release_lock
}

# What an interrupted run left behind: a temporary pointer that was never
# renamed, and a staging or replaced release directory. Called under the
# lock, as the Rust updater's own sweep of the same names is.
sweep_leftovers() {
  for leftover in "$native_root"/.current.* "$native_root"/.previous.* \
    "$native_root/releases"/.staging-* "$native_root/releases"/.replaced-*; do
    [ -e "$leftover" ] || continue
    rm -rf "$leftover" 2>/dev/null || :
  done
}

# Link `<bin>/bake` to this release's executable, through a temporary link
# and a rename, so a reader sees one command or the other.
write_command_link() {
  command_temp="$bin_dir/.bake.$$"
  rm -f "$command_temp"
  ln -s "$native_root/current/bin/$executable_name" "$command_temp"
  mv $pointer_mv "$command_temp" "$1"
}

# Create `<bin>/bake`, taking over a Bake-managed 0.3 command and recording
# every command this installer created or replaced in links.json. `dsh` and
# the 0.3 install are never touched.
link_command() {
  command_path="$bin_dir/bake"
  case $command_path in
    *[[:cntrl:]]*) fail "cannot record the command $command_path, which holds a control character" ;;
  esac
  owner=$(command_owner "$command_path")
  if [ "$owner" = native ]; then
    # A rerun of this installer, which is how an install is updated. The
    # command already leads to `current`, and links.json still records what
    # the first run replaced; recording this install's own link instead
    # would lose the 0.3 command `--uninstall-links` has to restore. The
    # link is rewritten only when the release renamed its executable.
    if [ "$(readlink "$command_path")" != "$native_root/current/bin/$executable_name" ]; then
      write_command_link "$command_path"
    fi
    step "Kept $command_path"
    return 0
  fi
  replaced='null'
  case $owner in
    none) ;;
    legacy)
      step "Taking over $command_path from the Bake 0.3 install"
      replaced="{\"kind\":\"symlink\",\"target\":\"$(json_value "$(readlink "$command_path")")\"}"
      ;;
    foreign)
      if [ -z "$replace_command" ]; then
        printf 'Bake install: %s exists and no Bake installer made it, so it was left alone.\n' "$command_path" >&2
        printf 'Bake %s is installed at %s. Run it there, or install again with --replace-command (BAKE_REPLACE_COMMAND=1) to replace that file and keep a backup.\n' "$version" "$destination" >&2
        exit 1
      fi
      mkdir -p "$native_root/backups"
      backup="$native_root/backups/bake.$(date +%s).$$"
      mv -f "$command_path" "$backup"
      step "Replaced $command_path, kept as $backup"
      replaced="{\"kind\":\"file\",\"backup\":\"$(json_value "$backup")\"}"
      ;;
  esac
  write_command_link "$command_path"
  links_temp="$native_root/.links.json.$$"
  {
    printf '%s\n' '{"schema":"bake-native-links/1","links":['
    printf '  {"path":"%s","replaced":%s}\n' "$(json_value "$command_path")" "$replaced"
    printf '%s\n' ']}'
  } > "$links_temp"
  mv -f "$links_temp" "$native_root/links.json"
  step "Linked $command_path"
}

need tar || fail 'this install needs tar'
need curl || need wget || fail 'this install needs curl or wget'
need sha256sum || need shasum || fail 'this install needs sha256sum or shasum'

if [ "$mode" = uninstall-links ]; then
  uninstall_links
  exit 0
fi

install_release
case ":$PATH:" in
  *":$bin_dir:"*) step 'Run: bake' ;;
  *) step "Add $bin_dir to PATH, then run: bake" ;;
esac
