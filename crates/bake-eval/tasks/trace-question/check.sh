set -eu
if ! diff -r -x __pycache__ . "$BAKE_EVAL_TASK_DIR/workspace" >/dev/null; then
    echo "the workspace was changed"
    exit 1
fi
for needle in '_apply_volume_break' 'discounts\.py' '(^|[^0-9])12([^0-9]|$)'; do
    if ! grep -Eq -e "$needle" "$BAKE_EVAL_ANSWER"; then
        echo "the answer does not match $needle"
        exit 1
    fi
done
