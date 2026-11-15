set -eu
for file in tests/data/sales.csv tests/data/sales.jsonl examples/by-region.spec; do
    if ! cmp -s "$file" "$BAKE_EVAL_TASK_DIR/workspace/$file"; then
        echo "$file was changed"
        exit 1
    fi
done
if ! grep -q default_registry README.md; then
    echo "README.md does not describe default_registry"
    exit 1
fi
if ! output=$(python3 "$BAKE_EVAL_TASK_DIR/hidden_test.py" 2>&1); then
    printf '%s\n' "$output" | tail -n 8
    exit 1
fi
if ! output=$(python3 -m unittest discover -s tests -t . 2>&1); then
    printf '%s\n' "$output" | tail -n 12
    exit 1
fi
ran=$(printf '%s\n' "$output" | sed -n 's/^Ran \([0-9]*\) tests\{0,1\} in .*/\1/p')
if [ "${ran:-0}" -lt 21 ]; then
    echo "the unit tests ran ${ran:-0} tests; the project had 21"
    exit 1
fi
