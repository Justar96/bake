set -eu
for file in AUTHORS CODENAMES.txt README.md RELEASING.md tidy/format.py tidy/__main__.py; do
    if ! cmp -s "$file" "$BAKE_EVAL_TASK_DIR/workspace/$file"; then
        echo "$file was changed"
        exit 1
    fi
done
if ! python3 "$BAKE_EVAL_TASK_DIR/gen_log.py" | cmp -s - logs/commits.log; then
    echo "logs/commits.log was changed"
    exit 1
fi
if ! diff -r "$HOME/.agents" "$BAKE_EVAL_TASK_DIR/home/.agents" >/dev/null 2>&1; then
    echo "the skills in the home directory were changed"
    exit 1
fi
python3 "$BAKE_EVAL_TASK_DIR/check.py"
