set -eu
if ! diff -r -x __pycache__ tests "$BAKE_EVAL_TASK_DIR/workspace/tests" >/dev/null; then
    echo "the tests or their data were changed"
    exit 1
fi
if ! diff -r docs "$BAKE_EVAL_TASK_DIR/workspace/docs" >/dev/null; then
    echo "docs/ was changed"
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
