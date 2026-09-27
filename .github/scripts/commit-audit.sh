#!/usr/bin/env bash
# Commit identity audit.
#
# Scans the full history reachable from a ref (default: HEAD) and fails if any
# commit uses an author or committer email that is not present in the allowlist
# (.github/allowed-commit-emails.txt).
#
# Why: GitHub attributes commits to user accounts BY EMAIL. A made-up address
# like <someone-elses-login>@users.noreply.github.com silently credits a real,
# unrelated account and adds a phantom external contributor to the repo.
#
# Usage:  bash .github/scripts/commit-audit.sh [git-ref]
# Env:    ALLOWLIST_FILE  override allowlist path (default .github/allowed-commit-emails.txt)
set -euo pipefail

ALLOWLIST_FILE="${ALLOWLIST_FILE:-.github/allowed-commit-emails.txt}"
REF="${1:-HEAD}"

if [[ ! -f "$ALLOWLIST_FILE" ]]; then
  echo "::error file=${ALLOWLIST_FILE}::allowlist file not found (cwd: $(pwd))"
  exit 1
fi

# Load allowlist: strip comments and surrounding whitespace, lowercase, drop empties.
mapfile -t ALLOWED < <(sed -e 's/#.*//' "$ALLOWLIST_FILE" \
  | tr '[:upper:]' '[:lower:]' \
  | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' \
  | { grep -v '^$' || true; } | sort -u)

if (( ${#ALLOWED[@]} == 0 )); then
  echo "::error file=${ALLOWLIST_FILE}::allowlist is empty — refusing to audit against nothing"
  exit 1
fi

echo "Auditing history of ref: $REF"
echo "Allowlist ($ALLOWLIST_FILE):"
printf '  - %s\n' "${ALLOWED[@]}"
echo

is_allowed() {
  local email="$1" candidate
  for candidate in "${ALLOWED[@]}"; do
    [[ "$email" == "$candidate" ]] && return 0
  done
  return 1
}

total=0
violations=0

while IFS=$'\x1f' read -r sha an ae cn ce subj; do
  total=$((total + 1))
  ae_lc="$(printf '%s' "$ae" | tr '[:upper:]' '[:lower:]')"
  ce_lc="$(printf '%s' "$ce" | tr '[:upper:]' '[:lower:]')"

  if ! is_allowed "$ae_lc"; then
    violations=$((violations + 1))
    echo "::error title=Disallowed author identity::Commit ${sha:0:12} \"${subj}\" has author '${an} <${ae}>' which is not in ${ALLOWLIST_FILE}. This email may attribute the commit to an unrelated GitHub account."
  fi
  if ! is_allowed "$ce_lc"; then
    violations=$((violations + 1))
    echo "::error title=Disallowed committer identity::Commit ${sha:0:12} \"${subj}\" has committer '${cn} <${ce}>' which is not in ${ALLOWLIST_FILE}. This email may attribute the commit to an unrelated GitHub account."
  fi
done < <(git log "$REF" --format='%H%x1f%an%x1f%ae%x1f%cn%x1f%ce%x1f%s')

echo "Audited ${total} commit(s); found ${violations} identity violation(s)."

if (( violations > 0 )); then
  cat <<'GUIDANCE'

HOW TO FIX
  * Not pushed yet:
      git config user.name  "OrientCOMPASS"
      git config user.email "orientcompass@users.noreply.github.com"
      git commit --amend --reset-author --no-edit        # or rebase for older commits
  * Already pushed:
      Rewrite the offending commits (git rebase / git filter-repo --mailmap),
      then push with --force-with-lease. Afterwards trigger a contributor-stats
      refresh: GET /repos/{owner}/{repo}/stats/contributors until it returns 200.
  * Adding a new legitimate identity (human or bot):
      Add its email to .github/allowed-commit-emails.txt in the SAME change.
  * NEVER invent an email address: GitHub maps <login>@users.noreply.github.com
    to the real account owning <login>, creating a phantom contributor.
GUIDANCE
  exit 1
fi

echo "OK: every commit uses an allowed identity."
