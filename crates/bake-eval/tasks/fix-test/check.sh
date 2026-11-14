set -eu
if ! cmp -s tests/test_stock.py "$BAKE_EVAL_TASK_DIR/workspace/tests/test_stock.py"; then
    echo "tests/test_stock.py was changed"
    exit 1
fi
if ! output=$(python3 -m unittest discover -s tests -t . 2>&1); then
    printf '%s\n' "$output" | tail -n 12
    exit 1
fi
