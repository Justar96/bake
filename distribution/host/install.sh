#!/bin/sh
set -eu

base_url=${BAKE_RELEASE_BASE_URL:-https://bake.justar.dev}
install_root=${BAKE_INSTALL_ROOT:-"$HOME/.local/share/bake"}
bin_dir=${BAKE_BIN_DIR:-"$HOME/.local/bin"}

command -v curl >/dev/null 2>&1 || { echo 'Bake install needs curl.' >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { echo 'Bake install needs tar.' >&2; exit 1; }
command -v node >/dev/null 2>&1 || { echo 'Bake install needs Node.js 24 or newer.' >&2; exit 1; }
node -e 'process.exit(Number(process.versions.node.split(".")[0]) >= 24 ? 0 : 1)' || {
  echo 'Bake install needs Node.js 24 or newer.' >&2
  exit 1
}

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=darwin-arm64 ;;
  Darwin-x86_64) target=darwin-x64 ;;
  Linux-aarch64|Linux-arm64) target=linux-arm64 ;;
  Linux-x86_64) target=linux-x64 ;;
  *) echo 'This platform has no Bake release archive.' >&2; exit 1 ;;
esac

temporary=$(mktemp -d "${TMPDIR:-/tmp}/bake-install.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
curl -fsSL "$base_url/latest.json" -o "$temporary/latest.json"
node - "$temporary/latest.json" "$target" > "$temporary/selected" <<'NODE'
const fs = require('node:fs')
const manifest = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'))
const target = process.argv[3]
const artifact = manifest.artifacts?.[target]
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(manifest.version)
  || artifact?.file !== `bake-v${manifest.version}-${target}.tar.gz`
  || !/^[a-f0-9]{64}$/.test(artifact.sha256)) {
  console.error(`No valid Bake archive for ${target}`)
  process.exit(1)
}
console.log(manifest.version)
console.log(artifact.file)
console.log(artifact.sha256)
NODE
version=$(sed -n '1p' "$temporary/selected")
file=$(sed -n '2p' "$temporary/selected")
expected=$(sed -n '3p' "$temporary/selected")
curl -fsSL "$base_url/releases/$version/$file" -o "$temporary/$file"
if command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$temporary/$file" | cut -d ' ' -f 1)
elif command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$temporary/$file" | cut -d ' ' -f 1)
else
  echo 'Bake install needs shasum or sha256sum.' >&2
  exit 1
fi
if [ "$actual" != "$expected" ]; then
  echo 'Bake download checksum did not match the release manifest.' >&2
  exit 1
fi

mkdir -p "$install_root/versions" "$bin_dir" "$temporary/unpacked"
tar -xzf "$temporary/$file" -C "$temporary/unpacked"
node "$temporary/unpacked/apps/cli/lib/bin.js" --version >/dev/null
digest_prefix=$(printf '%s' "$expected" | cut -c 1-12)
new_version="$install_root/versions/$version-$digest_prefix"
if [ -d "$new_version" ]; then
  node "$new_version/apps/cli/lib/bin.js" --version >/dev/null
else
  mv "$temporary/unpacked" "$new_version"
fi
ln -s "$new_version" "$install_root/.current-$$"
if [ -L "$install_root/current" ]; then
  rm "$install_root/current"
elif [ -e "$install_root/current" ]; then
  echo "$install_root/current exists and is not a Bake-managed link." >&2
  exit 1
fi
mv -f "$install_root/.current-$$" "$install_root/current"
ln -s "$install_root/current/bin/bake" "$bin_dir/.bake-$$"
if [ -L "$bin_dir/bake" ]; then
  rm "$bin_dir/bake"
elif [ -e "$bin_dir/bake" ]; then
  echo "$bin_dir/bake exists and is not a Bake-managed link." >&2
  exit 1
fi
mv -f "$bin_dir/.bake-$$" "$bin_dir/bake"

echo "Installed Bake $version at $new_version"
case ":$PATH:" in
  *":$bin_dir:"*) echo 'Run: bake' ;;
  *) echo "Add $bin_dir to PATH, then run: bake" ;;
esac
