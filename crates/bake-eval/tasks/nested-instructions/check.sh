set -eu
template="$BAKE_EVAL_TASK_DIR/workspace"
for path in AGENTS.md shop services/orders services/billing tools tests/orders tests/billing \
    services/catalog/catalog.py services/catalog/stock.py services/catalog/handlers/AGENTS.md \
    services/catalog/handlers/base.py tests/catalog/helpers.py; do
    if ! diff -r -x __pycache__ -x data "$path" "$template/$path" >/dev/null; then
        echo "$path was changed"
        exit 1
    fi
done
for name in get_categories get_category_products get_health get_products get_search; do
    for path in "services/catalog/handlers/$name.py" "tests/catalog/test_$name.py"; do
        if ! cmp -s "$path" "$template/$path"; then
            echo "$path was changed"
            exit 1
        fi
    done
done
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
python3 "$BAKE_EVAL_TASK_DIR/gen_data.py" "$scratch/data" "$scratch/docs"
if ! diff -r shop/data "$scratch/data" >/dev/null; then
    echo "shop/data was changed"
    exit 1
fi
python3 "$BAKE_EVAL_TASK_DIR/check.py"
if ! output=$(python3 -m unittest discover -s tests -t . 2>&1); then
    printf '%s\n' "$output" | tail -n 12
    exit 1
fi
