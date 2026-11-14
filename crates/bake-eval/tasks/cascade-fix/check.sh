set -eu
if ! cmp -s verify.py "$BAKE_EVAL_TASK_DIR/workspace/verify.py"; then
    echo "verify.py was changed"
    exit 1
fi
python3 verify.py
