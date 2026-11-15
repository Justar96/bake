set -eu
mkdir -p logs
python3 "$BAKE_EVAL_TASK_DIR/gen_log.py" > logs/commits.log
