#!/usr/bin/awk -f
{
    col = 3;
    if (!size) size = 5;
    mod = NR%size;
    if (NR <= size) {
	count++;
    }
    else {
	sum -=array[mod];
    };
    sum += $(col);
    array[mod] = $(col);
    print $1 "," sum/count;
}
