set -eu
if ! diff -r "$HOME/.agents" "$BAKE_EVAL_TASK_DIR/home/.agents" >/dev/null 2>&1; then
    echo "the skills in the home directory were changed"
    exit 1
fi
python3 "$BAKE_EVAL_TASK_DIR/check.py"
