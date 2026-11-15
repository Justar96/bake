set -eu
if ! diff -r -x __pycache__ tests "$BAKE_EVAL_TASK_DIR/workspace/tests" >/dev/null; then
    echo "the tests were changed"
    exit 1
fi
if ! cmp -s examples/nightly.jobs "$BAKE_EVAL_TASK_DIR/workspace/examples/nightly.jobs"; then
    echo "examples/nightly.jobs was changed"
    exit 1
fi
limit="$BAKE_EVAL_TASK_DIR/limit.py"
if ! output=$(python3 "$limit" 40 python3 "$BAKE_EVAL_TASK_DIR/hidden_test.py" 2>&1); then
    printf '%s\n' "$output" | tail -n 8
    exit 1
fi
if ! output=$(python3 "$limit" 30 python3 -m unittest discover -s tests -t . 2>&1); then
    printf '%s\n' "$output" | tail -n 12
    exit 1
fi
