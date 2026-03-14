#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

ACTION="${1:-}"
if [[ -n "${ACTION}" ]]; then
    shift
fi

CONFIG="${REPO_ROOT}/config/config.toml"
ROOT="../HazardSystem"
PROCESSES=4
JOBS=8
VERBOSE=1
CHECK=0
ROLLBACK_AFTER_RUN=0
DRY_RUN=0
ONLY_PATTERNS=()
EXTRA_ARGS=()

usage() {
    cat <<'EOF'
Usage:
  scripts/feedback.sh <action> [options] [-- extra-formatter-args]

Actions:
  run              Run formatter and print summary/failures.
  restore-latest   Restore files from the latest backup manifest.
  cycle            Run formatter, then optionally rollback from latest manifest.
  status           Show latest manifest, latest report summary, and recent failures.

Options:
  --config <path>       Formatter config path. Default: config/config.toml
  --root <path>         Target project root. Default: ../HazardSystem
  --processes <n>       Number of processes. Default: 4
  --jobs <n>            Total jobs. Default: 8
  --check               Run formatter in check mode.
  --quiet               Disable --verbose on formatter run.
  --rollback            For cycle: restore latest manifest after run.
  --dry-run             For restore: print actions without copying files.
  --only <substring>    Restore only files whose source path contains substring.
                        Can be repeated.
  -h, --help            Show this help.

Examples:
  scripts/feedback.sh run --root ../HazardSystem --processes 4 --jobs 8
  scripts/feedback.sh restore-latest --only BitmaskTable.hpp
  scripts/feedback.sh cycle --root ../HazardSystem --rollback
EOF
}

abs_path() {
    local path="$1"
    if [[ "${path}" = /* ]]; then
        printf '%s\n' "${path}"
    else
        printf '%s\n' "${REPO_ROOT}/${path}"
    fi
}

config_string_value() {
    local key="$1"
    local value
    value="$(sed -n "s|^[[:space:]]*${key}[[:space:]]*=[[:space:]]*\"\\([^\"]*\\)\".*|\\1|p" "${CONFIG}" | head -n 1)"
    printf '%s\n' "${value}"
}

backup_dir_from_config() {
    local raw
    raw="$(config_string_value "backup_dir")"
    if [[ -z "${raw}" ]]; then
        printf '%s\n' "${REPO_ROOT}/var/backups"
        return
    fi
    abs_path "${raw}"
}

report_path_from_config() {
    local raw
    raw="$(config_string_value "report_path")"
    if [[ -z "${raw}" ]]; then
        printf '%s\n' "${REPO_ROOT}/var/reports/run.ndjson"
        return
    fi
    abs_path "${raw}"
}

latest_manifest_path() {
    local backup_dir="$1"
    if [[ ! -d "${backup_dir}" ]]; then
        return 1
    fi

    local candidate
    candidate="$(
        find "${backup_dir}" -mindepth 1 -maxdepth 1 -type d -printf '%T@|%p\n' 2>/dev/null \
            | sort -t '|' -nr \
            | while IFS='|' read -r _ts dir; do
                if [[ -f "${dir}/backup_manifest.toml" ]]; then
                    printf '%s\n' "${dir}/backup_manifest.toml"
                    break
                fi
            done
    )"
    if [[ -z "${candidate}" ]]; then
        return 1
    fi
    printf '%s\n' "${candidate}"
}

should_restore_source() {
    local source="$1"
    if [[ "${#ONLY_PATTERNS[@]}" -eq 0 ]]; then
        return 0
    fi
    local needle
    for needle in "${ONLY_PATTERNS[@]}"; do
        if [[ "${source}" == *"${needle}"* ]]; then
            return 0
        fi
    done
    return 1
}

restore_from_manifest() {
    local manifest="$1"
    local restored=0
    local skipped=0
    local missing_backup=0
    local source=""

    while IFS= read -r line || [[ -n "${line}" ]]; do
        case "${line}" in
            source\ =\ \"*\")
                source="${line#source = \"}"
                source="${source%\"}"
                ;;
            backup\ =\ \"*\")
                local backup
                backup="${line#backup = \"}"
                backup="${backup%\"}"
                if [[ -z "${source}" ]]; then
                    continue
                fi
                if ! should_restore_source "${source}"; then
                    ((skipped += 1))
                    source=""
                    continue
                fi
                if [[ ! -f "${backup}" ]]; then
                    printf 'missing backup: %s\n' "${backup}"
                    ((missing_backup += 1))
                    source=""
                    continue
                fi
                if [[ "${DRY_RUN}" -eq 1 ]]; then
                    printf '[dry-run] restore %s <- %s\n' "${source}" "${backup}"
                else
                    mkdir -p "$(dirname "${source}")"
                    cp -- "${backup}" "${source}"
                    printf 'restored: %s\n' "${source}"
                fi
                ((restored += 1))
                source=""
                ;;
        esac
    done <"${manifest}"

    printf 'restore_summary: restored=%d skipped=%d missing_backup=%d manifest=%s\n' \
        "${restored}" "${skipped}" "${missing_backup}" "${manifest}"
}

parse_run_summary() {
    local log_path="$1"
    printf 'summary:\n'
    grep -E '^(files processed|files changed|changed|errors|warnings|violations|backups):' "${log_path}" || true
}

parse_recent_errors_from_log() {
    local log_path="$1"
    local found
    found="$(grep -E '^(error:|ERROR )' "${log_path}" || true)"
    if [[ -n "${found}" ]]; then
        printf 'errors_from_run:\n%s\n' "${found}"
    fi
    local missing_compdb
    missing_compdb="$(
        sed -n 's/.*requires compile_commands entry for \(.*\)$/\1/p' "${log_path}" \
            | sort -u
    )"
    if [[ -n "${missing_compdb}" ]]; then
        printf 'missing_compile_commands_entries:\n%s\n' "${missing_compdb}"
    fi
}

parse_errors_from_report() {
    local report_path="$1"
    if [[ ! -f "${report_path}" ]]; then
        return
    fi
    local extracted
    extracted="$(sed -n 's/.*"path":"\([^"]*\)".*"error":"\([^"]*\)".*/\1\t\2/p' "${report_path}" || true)"
    if [[ -n "${extracted}" ]]; then
        printf 'errors_from_report:\n%s\n' "${extracted}"
    fi
}

run_formatter() {
    local report_path backup_dir run_dir timestamp log_path
    report_path="$(report_path_from_config)"
    backup_dir="$(backup_dir_from_config)"
    run_dir="${REPO_ROOT}/var/runs"
    mkdir -p "${run_dir}"
    timestamp="$(date +%Y%m%d_%H%M%S)"
    log_path="${run_dir}/feedback_${timestamp}.log"

    local -a cmd
    cmd=(
        cargo run -q --
        --config "${CONFIG}"
        --root "${ROOT}"
        --processes "${PROCESSES}"
        --jobs "${JOBS}"
    )
    if [[ "${CHECK}" -eq 1 ]]; then
        cmd+=(--check)
    fi
    if [[ "${VERBOSE}" -eq 1 ]]; then
        cmd+=(--verbose)
    fi
    if [[ "${#EXTRA_ARGS[@]}" -gt 0 ]]; then
        cmd+=("${EXTRA_ARGS[@]}")
    fi

    printf 'run_command:'
    printf ' %q' "${cmd[@]}"
    printf '\n'
    printf 'log_path: %s\n' "${log_path}"

    set +e
    (
        cd "${REPO_ROOT}"
        "${cmd[@]}" 2>&1 | tee "${log_path}"
    )
    local status=$?
    set -e

    parse_run_summary "${log_path}"
    parse_recent_errors_from_log "${log_path}"
    parse_errors_from_report "${report_path}"

    local manifest
    manifest="$(latest_manifest_path "${backup_dir}" || true)"
    if [[ -n "${manifest}" ]]; then
        printf 'latest_manifest: %s\n' "${manifest}"
    else
        printf 'latest_manifest: <none>\n'
    fi
    printf 'report_path: %s\n' "${report_path}"
    printf 'run_status: %d\n' "${status}"
    return "${status}"
}

show_status() {
    local report_path backup_dir manifest
    report_path="$(report_path_from_config)"
    backup_dir="$(backup_dir_from_config)"
    manifest="$(latest_manifest_path "${backup_dir}" || true)"
    if [[ -n "${manifest}" ]]; then
        printf 'latest_manifest: %s\n' "${manifest}"
        printf 'manifest_meta:\n'
        sed -n '1,20p' "${manifest}"
    else
        printf 'latest_manifest: <none>\n'
    fi
    printf 'report_path: %s\n' "${report_path}"
    if [[ -f "${report_path}" ]]; then
        printf 'report_summary_path: %s\n' "${report_path%.jsonl}.summary.json"
        parse_errors_from_report "${report_path}"
    else
        printf 'report_missing: %s\n' "${report_path}"
    fi
}

if [[ -z "${ACTION}" ]]; then
    usage
    exit 2
fi

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --config)
            CONFIG="$(abs_path "$2")"
            shift 2
            ;;
        --root)
            ROOT="$2"
            shift 2
            ;;
        --processes)
            PROCESSES="$2"
            shift 2
            ;;
        --jobs)
            JOBS="$2"
            shift 2
            ;;
        --check)
            CHECK=1
            shift
            ;;
        --quiet)
            VERBOSE=0
            shift
            ;;
        --rollback)
            ROLLBACK_AFTER_RUN=1
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        --only)
            ONLY_PATTERNS+=("$2")
            shift 2
            ;;
        --)
            shift
            EXTRA_ARGS+=("$@")
            break
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            printf 'unknown option: %s\n\n' "$1"
            usage
            exit 2
            ;;
    esac
done

if [[ ! -f "${CONFIG}" ]]; then
    printf 'config_missing: %s\n' "${CONFIG}"
    exit 2
fi

case "${ACTION}" in
    run)
        run_formatter
        ;;
    restore-latest)
        backup_dir="$(backup_dir_from_config)"
        manifest="$(latest_manifest_path "${backup_dir}" || true)"
        if [[ -z "${manifest}" ]]; then
            printf 'no backup manifest found under %s\n' "${backup_dir}"
            exit 1
        fi
        restore_from_manifest "${manifest}"
        ;;
    cycle)
        run_formatter
        if [[ "${ROLLBACK_AFTER_RUN}" -eq 1 ]]; then
            backup_dir="$(backup_dir_from_config)"
            manifest="$(latest_manifest_path "${backup_dir}" || true)"
            if [[ -z "${manifest}" ]]; then
                printf 'no backup manifest found under %s\n' "${backup_dir}"
                exit 1
            fi
            restore_from_manifest "${manifest}"
        fi
        ;;
    status)
        show_status
        ;;
    *)
        printf 'unknown action: %s\n\n' "${ACTION}"
        usage
        exit 2
        ;;
esac
