#!/usr/bin/env bash
set -eu
set -o pipefail

RRD=test.rrd

MIN=4
HOUR=$((4 * 60))
SIXM=$((60 * 24 * 180))
if true; then
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
    sed 's/,/ /g' t.csv | while read T NR W KWH BAT STATUS; do
	WH=$(echo "$KWH * 1000" | bc -l | sed -e 's/[.]0*$//')
	S="$T:$W:$WH"
	#echo $S
	echo "${S?}"
    done | xargs rrdtool update "${RRD?}"
fi
	
exec rrdtool graph test.png \
	-z \
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
