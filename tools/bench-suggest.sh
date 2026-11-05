#!/bin/sh
# Times one `kbx suggest --judge none` and splits it into qmd's work and kbx's.
#
#   tools/bench-suggest.sh <kbx binary> <vault> <label> [real|fake]
#
# `real` puts a wrapper called qmd first on PATH, which runs the real qmd under
# /usr/bin/time and logs each call. `fake` answers every call at once with no
# hits, so what's left of the run is kbx. See docs/embedding-cache.md.
set -eu

kbx=$1
vault=$2
label=$3
mode=${4:-real}

out=$(dirname "$vault")/logs/$label
mkdir -p "$out/bin"
: > "$out/calls"

if [ "$mode" = real ]; then
    real=$(command -v qmd)
    cat > "$out/bin/qmd" <<EOF
#!/bin/sh
spent=\$(mktemp)
status=0
/usr/bin/time -p -o "\$spent" '$real' "\$@" || status=\$?
echo "\$3 \$(tr '\n' ' ' < "\$spent")" >> '$out/calls'
rm -f "\$spent"
exit \$status
EOF
else
    cat > "$out/bin/qmd" <<EOF
#!/bin/sh
echo "\$3 real 0 user 0 sys 0" >> '$out/calls'
case "\$3" in query|search) echo '[]' ;; esac
EOF
fi
chmod +x "$out/bin/qmd"

cd "$vault"
PATH="$out/bin:$PATH" /usr/bin/time -l "$kbx" suggest --judge none \
    > "$out/stdout" 2> "$out/time"

awk -v label="$label" '
    /[0-9.]+ real/ { wall = $1; user = $3; sys = $5 }
    /maximum resident/ { rss = $1 / 1048576 }
    END { printf "%s: wall %.1fs, cpu %.1fs (user %.1f sys %.1f), kbx peak %d MiB\n",
          label, wall, user + sys, user, sys, rss }
' "$out/time"
awk '
    { n[$1]++; wall[$1] += $3; cpu[$1] += $5 + $7; total += $3; totalcpu += $5 + $7 }
    END {
        for (c in n) printf "  qmd %-8s %4d calls, wall %8.1fs, cpu %8.1fs\n", c, n[c], wall[c], cpu[c]
        printf "  qmd total    %4d calls, wall %8.1fs, cpu %8.1fs\n", NR, total, totalcpu
    }
' "$out/calls"
grep -E 'candidates$' "$out/stdout" || true
