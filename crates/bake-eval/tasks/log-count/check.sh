set -eu
if ! diff -r -x __pycache__ -x app.log . "$BAKE_EVAL_TASK_DIR/workspace" >/dev/null; then
    echo "the workspace was changed"
    exit 1
fi
python3 "$BAKE_EVAL_TASK_DIR/check.py"
