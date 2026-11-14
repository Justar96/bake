set -eu
id=$(sha256sum .seed | cut -c1-12)
grep -q "$id" "$BAKE_EVAL_ANSWER" || { echo "the answer does not hold the build id $id"; exit 1; }
