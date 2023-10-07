#!/usr/bin/env bash
set -eu
set -o pipefail

RRD="test.rrd"

MIN=4
HOUR=$((4 * 60))
SIXM=$((60 * 24 * 180))
if false; then
    echo "Creating rrd file…"
    rrdtool create "${RRD?}" \
	    --start "$(date +%s -d2023-01-01)" \
	    -s 15 \
	    DS:w:GAUGE:600:0:10000 \
	    DS:wh:COUNTER:600:0:10000000 \
	    RRA:MIN:0.5:${MIN?}:${SIXM?} \
	    RRA:MAX:0.5:${MIN?}:${SIXM?} \
	    RRA:AVERAGE:0.5:${MIN?}:${SIXM?} \
	    RRA:MIN:0.5:${HOUR?}:${SIXM?} \
	    RRA:MAX:0.5:${HOUR?}:${SIXM?} \
	    RRA:AVERAGE:0.5:${HOUR?}:${SIXM?}
fi

if true; then
    LAST="$(rrdtool last "${RRD?}")"
    echo "Adding data…"
    awk '
BEGIN {
  FS=","
  OFS=":"
}
$1 > '"${LAST?}"' {
  print $1, $3, $4 * 1000
}
' t.csv \
	| xargs rrdtool update "${RRD?}"
fi
echo "Graphing…"
exec rrdtool graph test.png \
	-g \
	-X 0 \
	-y '50:2' \
	-t 'Power consumption in watts' \
	-l 0 \
	-w 1280 \
	-h 720 \
	-s -7d \
	-e -1h \
	-a PNG \
	'DEF:bleh=test.rrd:w:AVERAGE:step=600:reduce=MIN' \
	'DEF:wh=test.rrd:wh:MIN' \
	'LINE2:bleh#000000:Watts'
