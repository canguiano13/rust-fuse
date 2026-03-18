#!/bin/bash
#run a series of fio benchmarks on a running filesystem

#reminder !
echo "MAKE SURE THE FS IS RUNNING."
sleep 1

read -p "ENTER PATH TO FS MOUNT POINT: " -e MNT_REL_PATH

#get absolute path to filesystem mountpoint
MNT_ABS_PATH=""

#exit if path is badly formed
if [[ $MNT_REL_PATH == "" ]]; then
    echo "error: bad path.\ntry again and supply a path to the fs mount point\n"
    exit -1
#use the absolute path
else 
    MNT_ABS_PATH=`realpath $MNT_REL_PATH`
    echo "benchmarking $MNT_ABS_PATH..."
fi

#validate path
if ! [[ -d $MNT_ABS_PATH ]]; then
    echo -n "error: $MNT_ABS_PATH does not exist.\n" >&2
    exit -1
fi

#set the flags for benchmarking
RAND_SEED="1"       #seed for random tests 
IO_ENGINE="libaio"  #use linux's async io engine
DIRECT="1"          #bypass kernel page cache to use the fs directly
RAMP_TIME="4"       #warm up time in seconds
COMMON_FLAGS="--randrepeat=${RAND_SEED} --ioengine=${IO_ENGINE} --direct=${DIRECT} --ramp_time=${RAMP_TIME}"

echo ""
echo "random write (iops):"
echo "------------------------------------"
#bs = block size for IOPS tets
#size = total data to read/write
sync; fio $COMMON_FLAGS --name=randwrite --bs=4k --size=1G --readwrite=randwrite

echo ""
echo "random read (iops):"
echo "------------------------------------"
sync; fio $COMMON_FLAGS --name=randread --bs=4k --size=1G --readwrite=randread

echo ""
echo "mixed read/write (iops)"
echo "------------------------------------"
sync; fio $COMMON_FLAGS --name=mixedrw --bs=4k --size=1G --readwrite=readwrite

echo ""
echo "sequential write (throughput)"
echo "------------------------------------"
sync; fio $COMMON_FLAGS --name=seqwrite --bs=4M --size=1G --readwrite=write

echo ""
echo "sequential read (throughput)"
echo "------------------------------------"
sync; fio $COMMON_FLAGS --name=seqread --bs=4M --size=1G --readwrite=read


