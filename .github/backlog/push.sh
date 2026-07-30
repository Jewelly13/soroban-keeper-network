#!/usr/bin/env bash
set -euo pipefail

if ! command -v gh >/dev/null 2>&1; then
  printf 'Error: gh CLI is required\n' >&2
  exit 1
fi

shopt -s nullglob
issues=(.github/backlog/issues/*.md)
if [[ "${#issues[@]}" -eq 0 ]]; then
  printf 'No backlog issues found.\n'
  exit 0
fi

for issue in "${issues[@]}"; do
  title="$(sed -n 's/^title: *"\(.*\)"$/\1/p' "$issue" | head -n 1)"
  if [[ -z "$title" ]]; then
    printf 'Error: no title found in %s\n' "$issue" >&2
    exit 1
  fi

  gh issue create --title "$title" --body-file "$issue"
done
