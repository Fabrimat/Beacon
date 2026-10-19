#!/usr/bin/env bash
# Maintainer-only local production-equivalence check for `beacon migrate`. Never run in CI.
# Same steps as fixtures-local.ps1. Usage:
#   scripts/fixtures-local.sh <regolith project dir> [-p|--profile packaging] [-e K=V]... [-n name] [-x|--pre "<pre-build cmd>"] [--dry-run]
# Copies the project to ${TMPDIR:-/tmp}/beacon-mig/<name> (source untouched, .regolith and
# filters_data/node_modules included), builds the oracle (+ the system_template evaluation)
# with Regolith in the copy, runs `beacon migrate --trust --json`, prints the report paths.
# Nothing is committed or written inside the repo.
set -euo pipefail
profile=packaging envs=() name="" dry=0 src="" pre=""
while [ $# -gt 0 ]; do
  case $1 in
    -p|--profile) profile=$2; shift 2;;
    -e|--env) envs+=("$2"); shift 2;;
    -n|--name) name=$2; shift 2;;
    -x|--pre) pre=$2; shift 2;;
    --dry-run|-WhatIf) dry=1; shift;;
    *) src=$1; shift;;
  esac
done
[ -d "$src" ] || { echo "usage: $0 <regolith project dir> [-p profile] [-e K=V] [-n name] [--dry-run]" >&2; exit 2; }
src=$(cd "$src" && pwd)
name=${name:-$(basename "$src")}
case $name in */*|*\*|*:*|.|..) echo "-n must not contain path separators" >&2; exit 2;; esac
tmp=${TMPDIR:-/tmp}/beacon-mig/$name copy=$tmp/project out=$tmp/migrated
repo=$(cd "$(dirname "$0")/.." && pwd)
case $tmp in "$repo"/*) echo "work dir is inside the repo" >&2; exit 2;; esac
step() { echo "==> $1"; }
run() { [ $dry = 1 ] || "$@"; }

# prints "st" when the profile uses system_template; with a 2nd arg "write", edits the copy's config.json
py=$(python3 -c 0 2>/dev/null && echo python3 || echo python)
cfg() { "$py" - "$1" "$profile" "$2" <<'PY'
import json, sys
path, prof, mode = sys.argv[1:4]
c = json.load(open(path, encoding="utf8"))
P = c["regolith"]["profiles"]
def flat(n):
    for f in P[n]["filters"]:
        yield from flat(f["profile"]) if "profile" in f else [f]
fl = list(flat(prof))
st = any(f.get("filter") == "system_template" for f in fl)
if mode == "write":
    ex = lambda d: {"target": "exact", "bpPath": f"./{d}/BP", "rpPath": f"./{d}/RP", "readOnly": False}
    P["beacon_oracle"] = {"filters": fl, "export": ex(".beacon-oracle")}
    if st:
        names = ["subfunctions", "system_template", "shapescape_custom_blocks_generator", "system_template_esbuild"]
        P["beacon_eval"] = {"filters": [f for n in names for f in fl if f.get("filter") == n][:len(names)], "export": ex(".beacon-eval")}
    json.dump(c, open(path, "w", encoding="utf8"), indent=2)
else:
    print("st" if st else "plain")
PY
}
st=$(cfg "$src/config.json" read)

step "copy $src -> $copy (never touches the source)"
run bash -c 'rm -rf "$1" && mkdir -p "$1" && cp -a "$2" "$3"' _ "$tmp" "$src" "$copy"
step "append profile beacon_oracle (filters of '$profile') to the copy's config.json"
run cfg "$copy/config.json" write
regolith_run() { ( cd "$copy" && env ${envs[@]+"${envs[@]}"} regolith run "$@" ); }
if [ -n "$pre" ]; then
  step "pre-build step in the copy: $pre"
  run bash -c 'cd "$1" && eval "$2"' _ "$copy" "$pre"
fi
if [ $dry = 1 ] || [ ! -d "$copy/.regolith/cache/filters" ]; then
  step "regolith install-all (if the copy has no filter cache)"
  run bash -c 'cd "$1" && regolith install-all' _ "$copy"
fi
step "regolith run beacon_oracle (in the copy)"
run regolith_run beacon_oracle
margs=(migrate "$copy" --out "$out" --oracle "$copy/.beacon-oracle" --oracle-profile "$profile")
if [ "$st" = st ]; then
  step "regolith run beacon_eval --debug; keep tmp data/ and system_template_log.json in .beacon-eval"
  run regolith_run beacon_eval --debug
  [ $dry = 1 ] || { [ -d "$copy/.regolith/tmp/data" ] && cp -a "$copy/.regolith/tmp/data" "$copy/.beacon-eval/data"
    [ -f "$copy/system_template_log.json" ] && cp "$copy/system_template_log.json" "$copy/.beacon-eval/"; true; }
  margs+=(--evaluated "$copy/.beacon-eval")
fi
for e in ${envs[@]+"${envs[@]}"}; do margs+=(--env "$e"); done
margs+=(--trust --json)
beacon=$(command -v beacon || echo "${BEACON_BIN:-$repo/target/debug/beacon}")
step "$beacon ${margs[*]} > $tmp/migrate.json"
if [ $dry = 1 ]; then echo "(dry run: nothing was copied, run or written)"; else
  code=0; "$beacon" "${margs[@]}" > "$tmp/migrate.json" || code=$?
  echo "equivalence summary: exit $code (non-zero = differences)"; head -c 2000 "$tmp/migrate.json"; echo
  echo "MIGRATION.md: $out/MIGRATION.md"; echo "diff list (full JSON report): $tmp/migrate.json"
  exit $code
fi
