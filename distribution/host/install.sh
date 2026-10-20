#!/bin/sh
# The Bake native installer: it installs the Rust agent, `bake`, from the
# signed native release channel. POSIX sh only, and the host needs no Node or
# Bun: it needs curl or wget, tar, sha256sum or shasum, and an OpenSSL with
# Ed25519 (OpenSSL 1.1.1 or newer).
#
#   curl -fsSL https://bake.justar.dev/install.sh | sh
#
# See distribution/README.md, "Native installers", for the route, the install
# layout, the `bake` takeover, and the lock these steps take.
set -eu

download_url=${BAKE_DOWNLOAD_URL:-https://bake.justar.dev}
download_url=${download_url%/}
native_root=${BAKE_NATIVE_ROOT:-"$HOME/.local/share/bake-rs"}
bin_dir=${BAKE_BIN_DIR:-"$HOME/.local/bin"}
# The 0.3 TypeScript install, which this installer reads to recognize a
# Bake-managed `bake` and never writes to.
legacy_root=${BAKE_INSTALL_ROOT:-"$HOME/.local/share/bake"}
replace_command=${BAKE_REPLACE_COMMAND:-}
# The native release key (distribution/host/native-release-key.pub), base64
# DER SubjectPublicKeyInfo. A release test holds every copy equal. Tests
# replace this one line in a private copy of the installer, so the published
# installer takes no key from its environment.
native_release_key='MCowBQYDK2VwAyEAI/nADgDPvSZRQnB+AwkqVzSuo8OcX1sV9PSt0SA0J1c='

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
  BAKE_NATIVE_ROOT    install root (default ~/.local/share/bake-rs)
  BAKE_BIN_DIR        command directory (default ~/.local/bin)
  BAKE_REPLACE_COMMAND=1  as --replace-command, for a piped install
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
    [ -d "$lock_dir" ] || return 1
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

# A path as a JSON string's content for links.json. Every backslash and quote
# becomes a `\uXXXX` escape, so an escaped value holds no raw quote or
# backslash and the reader can split a record on quotes.
json_value() {
  printf '%s' "$1" | sed 's/\\/\\u005c/g; s/"/\\u0022/g'
}

json_path() {
  printf '%s' "$1" | sed 's/\\u0022/"/g; s/\\u005c/\\/g'
}

# `0.4.1` is newer than `0.4.0` and than `0.4.1-rc.1`; a pre-release is older
# than its own release, and two pre-releases compare as text.
version_newer() {
  awk -v left="$1" -v right="$2" '
    function core(v) { sub(/-.*$/, "", v); return v }
    function pre(v) { return index(v, "-") ? substr(v, index(v, "-") + 1) : "" }
    BEGIN {
      split(core(left), a, ".")
      split(core(right), b, ".")
      for (i = 1; i <= 3; i++) {
        if (a[i] + 0 > b[i] + 0) { print "yes"; exit }
        if (a[i] + 0 < b[i] + 0) { print "no"; exit }
      }
      x = pre(left); y = pre(right)
      if (x == y) { print "no"; exit }
      if (x == "") { print "yes"; exit }
      if (y == "") { print "no"; exit }
      print (x > y) ? "yes" : "no"
    }
  ' | grep -q '^yes$'
}

# Set `target` to this host's release platform.
set_platform() {
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target=darwin-arm64 ;;
    Darwin-x86_64) target=darwin-x64 ;;
    Linux-aarch64 | Linux-arm64) target=linux-arm64 ;;
    Linux-x86_64) target=linux-x64 ;;
    *) fail "this platform has no Bake native release archive" ;;
  esac
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

# The executable inside an unpacked release. The archive's `bin/bake-rs`
# becomes `bin/bake`, so both names are accepted and `bin/bake` wins.
release_executable() {
  if [ -f "$1/bin/bake" ]; then
    printf '%s' "$1/bin/bake"
  elif [ -f "$1/bin/bake-rs" ]; then
    printf '%s' "$1/bin/bake-rs"
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
    *'"kind":"bake-rs-self-check"'*) ;;
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

# Every member of the archive, refused before anything is unpacked when it is
# absolute, holds a `..` component, or is anything but a regular file or a
# directory.
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
        rm -f "$path"
        ln -s "$target" "$path"
        step "Restored $path -> $target"
        restored=$((restored + 1))
        ;;
      *'"kind":"file"'*)
        backup=$(json_path "$(printf '%s' "$line" | cut -d '"' -f 14)")
        [ -e "$backup" ] || fail "the backup $backup recorded for $path is gone"
        rm -f "$path"
        mv -f "$backup" "$path"
        step "Restored $path from $backup"
        restored=$((restored + 1))
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

  step 'Verifying the native release manifest'
  fetch_to "$download_url/native/stable.json" "$temporary/stable.json" 60
  fetch_to "$download_url/native/stable.json.sig" "$temporary/stable.json.sig" 60
  {
    printf '%s\n' '-----BEGIN PUBLIC KEY-----'
    printf '%s\n' "$native_release_key" | fold -w 64
    printf '%s\n' '-----END PUBLIC KEY-----'
  } > "$temporary/native-release-key.pem"
  tr -d '[:space:]' < "$temporary/stable.json.sig" > "$temporary/signature.base64"
  # `openssl base64 -d` rejects nothing outside base64, so the text is checked
  # first and the decoded signature must be Ed25519's 64 bytes.
  grep -q '^[A-Za-z0-9+/]\{86\}==$' "$temporary/signature.base64" \
    || fail 'the native release manifest signature is not a base64 Ed25519 signature'
  openssl base64 -d -A -in "$temporary/signature.base64" -out "$temporary/signature.bin" 2>/dev/null \
    || fail 'the native release manifest signature is not base64'
  openssl pkeyutl -verify -pubin -inkey "$temporary/native-release-key.pem" -rawin \
    -in "$temporary/stable.json" -sigfile "$temporary/signature.bin" >/dev/null 2>&1 \
    || fail 'the native release manifest signature did not verify'

  # The manifest, as `bake-native-release/1` fixes it. Whitespace between
  # tokens is removed first; no member of this schema holds any.
  tr -d '[:space:]' < "$temporary/stable.json" > "$temporary/manifest"
  entry='"(darwin-arm64|darwin-x64|linux-arm64|linux-x64|win32-x64)":\{"file":"[A-Za-z0-9._-]+","sha256":"[0-9a-f]{64}","size":[0-9]+\}'
  grep -Eq '^\{"schema":"bake-native-release/1","channel":"stable","version":"[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?","published":"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z","artifacts":\{'"$entry"'(,'"$entry"')*\}\}$' \
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
  [ "$file" = "bake-rs-v$version-$target.tar.gz" ] || fail "the manifest names the archive $file, not bake-rs-v$version-$target.tar.gz"
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

# Create `<bin>/bake`, taking over a Bake-managed 0.3 command and recording
# every command this installer created or replaced in links.json. `dsh` and
# the 0.3 install are never touched.
link_command() {
  command_path="$bin_dir/bake"
  case $command_path in
    *[[:cntrl:]]*) fail "cannot record the command $command_path, which holds a control character" ;;
  esac
  owner=$(command_owner "$command_path")
  replaced='null'
  case $owner in
    none) ;;
    native)
      replaced="{\"kind\":\"symlink\",\"target\":\"$(json_value "$(readlink "$command_path")")\"}"
      ;;
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
  command_temp="$bin_dir/.bake.$$"
  rm -f "$command_temp"
  ln -s "$native_root/current/bin/$executable_name" "$command_temp"
  mv $pointer_mv "$command_temp" "$command_path"
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
need openssl || fail 'this install needs openssl 1.1.1 or newer, which verifies the release signature'
openssl list -public-key-algorithms 2>/dev/null | grep -qi ed25519 \
  || fail 'this openssl has no Ed25519, which the release signature needs; install openssl 1.1.1 or newer'

if [ "$mode" = uninstall-links ]; then
  uninstall_links
  exit 0
fi

install_release
case ":$PATH:" in
  *":$bin_dir:"*) step 'Run: bake' ;;
  *) step "Add $bin_dir to PATH, then run: bake" ;;
esac
