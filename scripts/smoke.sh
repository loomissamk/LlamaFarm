#!/usr/bin/env bash
# smoke.sh — five-tool validation matrix for LlamaFarm
#
# Runs the canonical smoke prompts for task_plan, web_search_tool, file_write,
# file_read, and python-via-shell against one or more models. Emits a
# structured pass/fail summary. Intended to be run before any release.
#
# Usage:
#   ./scripts/smoke.sh
#   ./scripts/smoke.sh --provider ollama --model qwen3.5:9b
#   ./scripts/smoke.sh --provider openrouter --model devstral-2:123b-cloud
#   ./scripts/smoke.sh --provider ollama --model qwen3.5:9b --provider openrouter --model devstral-2:123b-cloud
#   ./scripts/smoke.sh --timeout 90 --workspace /llamafarm-data/workspace
#
# Options:
#   --provider <name>    Provider name (default: ollama)
#   --model <id>         Model ID (default: qwen3.5:9b)
#   --timeout <secs>     Per-test timeout in seconds (default: 120)
#   --workspace <path>   Workspace path for file tests (default: /llamafarm-data/workspace)
#   --binary <path>      Path to llamafarm binary (default: auto-detect)
#   --dry-run            Print prompts without executing
#   --help               Show this message
#
# Exit code: 0 if all tests pass, 1 if any test fails.

set -euo pipefail

# ── defaults ──────────────────────────────────────────────────────────────────
DEFAULT_PROVIDER="ollama"
DEFAULT_MODEL="qwen3.5:9b"
TIMEOUT=120
WORKSPACE="/llamafarm-data/workspace"
BINARY=""
DRY_RUN=false

# ── parse args ────────────────────────────────────────────────────────────────
declare -a PROVIDERS=()
declare -a MODELS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --provider) PROVIDERS+=("$2"); shift 2 ;;
        --model)    MODELS+=("$2");    shift 2 ;;
        --timeout)  TIMEOUT="$2";      shift 2 ;;
        --workspace) WORKSPACE="$2";   shift 2 ;;
        --binary)   BINARY="$2";       shift 2 ;;
        --dry-run)  DRY_RUN=true;      shift   ;;
        --help)
            sed -n '/^# Usage:/,/^# Exit code:/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

if [[ ${#PROVIDERS[@]} -eq 0 ]]; then
    PROVIDERS=("$DEFAULT_PROVIDER")
    MODELS=("$DEFAULT_MODEL")
fi

# pair validation: must have equal counts
if [[ ${#PROVIDERS[@]} -ne ${#MODELS[@]} ]]; then
    echo "error: --provider and --model must be paired (got ${#PROVIDERS[@]} provider(s), ${#MODELS[@]} model(s))"
    exit 1
fi

# ── locate binary ─────────────────────────────────────────────────────────────
if [[ -z "$BINARY" ]]; then
    for candidate in \
        "$(command -v llamafarm 2>/dev/null)" \
        "./target/release/llamafarm" \
        "./target/debug/llamafarm" \
        "$HOME/.local/bin/llamafarm"
    do
        if [[ -x "$candidate" ]]; then
            BINARY="$candidate"
            break
        fi
    done
fi

if [[ -z "$BINARY" ]]; then
    echo "error: llamafarm binary not found. Build it first or pass --binary <path>."
    exit 1
fi

# ── colors ─────────────────────────────────────────────────────────────────────
if [[ -t 1 ]]; then
    GREEN="\033[32m"; RED="\033[31m"; YELLOW="\033[33m"
    BOLD="\033[1m"; RESET="\033[0m"
else
    GREEN=""; RED=""; YELLOW=""; BOLD=""; RESET=""
fi

# ── smoke prompts ─────────────────────────────────────────────────────────────
# Exact prompts from docs/tool-call-examples/README.md

PROMPT_TASK_PLAN="SMOKE ... task_plan. Use the task_plan tool to create a short 3-step plan for: write a file, read it, and delete it. Only create the plan and then stop. Use tools, not descriptions."

PROMPT_WEB_SEARCH="SMOKE ... web_search. Use the web_search_tool to search the web for the official Rust language website and return the main URL. Use tools, not descriptions."

PROMPT_FILE_WRITE="SMOKE ... file_write. Use file_write to write exactly \"tool smoke llamafarm\" to ${WORKSPACE}/tool_smoke_matrix.txt and then confirm success. Use tools, not descriptions."

PROMPT_FILE_READ="SMOKE ... file_read. Use file_read to read ${WORKSPACE}/tool_smoke_matrix.txt and return the exact contents. Use tools, not descriptions."

PROMPT_PYTHON="SMOKE ... python. Write a tiny Python file in ${WORKSPACE} that prints 2 + 2, run it with python3, delete it afterwards, and report the result. Use tools, not descriptions."

# ── per-test check functions ───────────────────────────────────────────────────
# Each function receives the captured output on stdin; returns 0=pass, 1=fail.

check_task_plan() {
    local out="$1"
    # must mention a numbered step and not just describe the action
    echo "$out" | grep -qiE "(step|task|1\.|2\.|3\.)" \
        && echo "$out" | grep -qiE "(write|read|delete|creat)" \
        && return 0
    return 1
}

check_web_search() {
    local out="$1"
    echo "$out" | grep -qF "rust-lang.org" && return 0
    return 1
}

check_file_write() {
    local out="$1"
    echo "$out" | grep -qi "success\|written\|wrote\|created" && return 0
    return 1
}

check_file_read() {
    local out="$1"
    # must echo back the content that was written
    echo "$out" | grep -qi "tool smoke llamafarm" && return 0
    return 1
}

check_python() {
    local out="$1"
    # output of print(2+2) is 4
    echo "$out" | grep -qE "\b4\b" && return 0
    return 1
}

# ── run one test ───────────────────────────────────────────────────────────────
# run_test <label> <check_fn> <prompt> <provider> <model>
# Returns 0=pass, 1=fail, 2=timeout, 3=error

run_test() {
    local label="$1"
    local check_fn="$2"
    local prompt="$3"
    local provider="$4"
    local model="$5"

    if [[ "$DRY_RUN" == "true" ]]; then
        echo "  [DRY-RUN] $label"
        echo "  prompt: $prompt"
        return 0
    fi

    local tmpfile
    tmpfile=$(mktemp /tmp/smoke_XXXXXX.txt)
    local exit_code=0

    # run llamafarm agent in single-message mode with timeout
    timeout "$TIMEOUT" "$BINARY" agent \
        --provider "$provider" \
        --model "$model" \
        --message "$prompt" \
        --memory-backend none \
        --autonomy-level full \
        > "$tmpfile" 2>&1 || exit_code=$?

    local output
    output=$(cat "$tmpfile")
    rm -f "$tmpfile"

    if [[ $exit_code -eq 124 ]]; then
        echo "$output"
        return 2  # timeout
    fi

    if [[ $exit_code -ne 0 ]]; then
        echo "$output"
        return 3  # error
    fi

    echo "$output"
    "$check_fn" "$output"
}

# ── result tracking ────────────────────────────────────────────────────────────
declare -A RESULTS  # "model:label" -> pass|fail|timeout|error|skip

record_result() {
    local model="$1" label="$2" status="$3"
    RESULTS["${model}:${label}"]="$status"
}

# ── run matrix ─────────────────────────────────────────────────────────────────
TESTS=(
    "task_plan|check_task_plan|$PROMPT_TASK_PLAN"
    "web_search_tool|check_web_search|$PROMPT_WEB_SEARCH"
    "file_write|check_file_write|$PROMPT_FILE_WRITE"
    "file_read|check_file_read|$PROMPT_FILE_READ"
    "python|check_python|$PROMPT_PYTHON"
)

TOTAL_PASS=0
TOTAL_FAIL=0
TOTAL_ERROR=0

for i in "${!PROVIDERS[@]}"; do
    provider="${PROVIDERS[$i]}"
    model="${MODELS[$i]}"

    echo ""
    echo -e "${BOLD}=== ${provider} / ${model} ===${RESET}"

    for test_spec in "${TESTS[@]}"; do
        IFS='|' read -r label check_fn prompt <<< "$test_spec"

        printf "  %-20s ... " "$label"

        captured_output=$(run_test "$label" "$check_fn" "$prompt" "$provider" "$model" 2>&1)
        test_result=$?

        case $test_result in
            0)
                echo -e "${GREEN}PASS${RESET}"
                record_result "$model" "$label" "pass"
                (( TOTAL_PASS++ ))
                ;;
            1)
                echo -e "${RED}FAIL${RESET} (check did not match)"
                echo "    output excerpt: $(echo "$captured_output" | tail -3 | head -1)"
                record_result "$model" "$label" "fail"
                (( TOTAL_FAIL++ ))
                ;;
            2)
                echo -e "${YELLOW}TIMEOUT${RESET} (>${TIMEOUT}s)"
                record_result "$model" "$label" "timeout"
                (( TOTAL_FAIL++ ))
                ;;
            3)
                echo -e "${RED}ERROR${RESET} (non-zero exit)"
                echo "    output excerpt: $(echo "$captured_output" | grep -i 'error\|fail\|panic' | head -1)"
                record_result "$model" "$label" "error"
                (( TOTAL_ERROR++ ))
                ;;
        esac
    done
done

# ── summary table ─────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}── Smoke Matrix Summary ──${RESET}"
printf "%-28s" "model"
for test_spec in "${TESTS[@]}"; do
    IFS='|' read -r label _ _ <<< "$test_spec"
    printf "%-18s" "$label"
done
echo ""

for i in "${!PROVIDERS[@]}"; do
    model="${MODELS[$i]}"
    printf "%-28s" "$model"
    for test_spec in "${TESTS[@]}"; do
        IFS='|' read -r label _ _ <<< "$test_spec"
        status="${RESULTS["${model}:${label}"]:-skip}"
        case "$status" in
            pass)    printf "${GREEN}%-18s${RESET}" "pass" ;;
            fail)    printf "${RED}%-18s${RESET}" "FAIL" ;;
            timeout) printf "${YELLOW}%-18s${RESET}" "TIMEOUT" ;;
            error)   printf "${RED}%-18s${RESET}" "ERROR" ;;
            *)       printf "%-18s" "skip" ;;
        esac
    done
    echo ""
done

echo ""
total=$(( TOTAL_PASS + TOTAL_FAIL + TOTAL_ERROR ))
echo -e "Passed: ${GREEN}${TOTAL_PASS}${RESET} / ${total}  |  Failed: ${RED}${TOTAL_FAIL}${RESET}  |  Errors: ${RED}${TOTAL_ERROR}${RESET}"

if [[ $TOTAL_FAIL -gt 0 || $TOTAL_ERROR -gt 0 ]]; then
    exit 1
fi
exit 0
