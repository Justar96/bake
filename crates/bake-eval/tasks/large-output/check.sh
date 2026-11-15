set -eu
if ! cmp -s build.py "$BAKE_EVAL_TASK_DIR/workspace/build.py"; then
    echo "build.py was changed"
    exit 1
fi
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
python3 "$BAKE_EVAL_TASK_DIR/gen_modules.py" "$scratch/modules"
if ! diff -r modules "$scratch/modules" >/dev/null; then
    echo "modules/ was changed"
    exit 1
fi
if ! output=$(python3 build.py 2>&1); then
    printf '%s\n' "$output" | tail -n 1
    echo "python3 build.py fails"
    exit 1
fi
if ! printf '%s\n' "$output" | tail -n 1 | grep -q '^build succeeded: 600 modules'; then
    echo "python3 build.py did not build the 600 modules"
    exit 1
fi
