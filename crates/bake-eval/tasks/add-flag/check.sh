set -eu
if ! output=$(python3 "$BAKE_EVAL_TASK_DIR/hidden_test.py" 2>&1); then
    printf '%s\n' "$output" | tail -n 8
    exit 1
fi
if ! grep -q -e '--words' tests/test_linecount.py; then
    echo "tests/test_linecount.py has no test of --words"
    exit 1
fi
if ! output=$(python3 -m unittest discover -s tests -t . 2>&1); then
    printf '%s\n' "$output" | tail -n 12
    exit 1
fi
