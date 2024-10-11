#!/usr/bin/env bash
set -eu
set -o pipefail

RRD="test.rrd"

MIN=4
HOUR=$((4 * 60))
SIXM=$((60 * 24 * 180))
if [ ! -f "${RRD?}" ]; then
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
    grep ',OK$' t.csv \
    | python3 <(cat <<EOF
import sys
for line in sys.stdin:
    e = line.split(',')
    if int(e[0]) > ${LAST?}:
      print('%s:%s:%d' % (e[0], e[2], int(float(e[3])*1000)))
EOF
) | tee awked.txt \
	| xargs -r rrdtool update "${RRD?}"
fi
echo "Graphing…"
for span in 1d 7d 180d 1y 2y; do
        rrdtool graph $span.png \
                -g \
                -X 0 \
                -y '50:2' \
                -t 'Power consumption in watts' \
                -l 0 \
                -w 1280 \
                -h 720 \
                -s -$span \
                -e -1h \
                -a PNG \
                'DEF:min=test.rrd:w:AVERAGE:step=600:reduce=MIN' \
                'DEF:avg=test.rrd:w:AVERAGE:step=600:reduce=AVERAGE' \
                'DEF:max=test.rrd:w:AVERAGE:step=600:reduce=MAX' \
                'DEF:wh=test.rrd:wh:MIN' \
                'LINE2:min#80ff80:Watts' \
                'LINE2:avg#000000:Watts'
done
