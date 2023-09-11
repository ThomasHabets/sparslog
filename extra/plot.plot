#!/usr/bin/env gnuplot

set terminal pngcairo size 1280,720 font "Helvetica"
set output 'plot.png'
set datafile separator ','
set ylabel "Watts"
set xlabel "Date"
set timefmt "%s"
set format x "%Y-%m-%d\n%H:%M:%S"
set xdata time
set xtics rotate
set grid
plot [] [0:500] 't.csv' using 1:3 w l
