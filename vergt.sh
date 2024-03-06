#!/bin/bash
IFS=.
ver1=($1)
ver2=($2)
# starting from minor of ver1 if ver1 shorter ver2
# fill absent fields in ver1 with zeros
for ((i=${#ver1[@]}; i<${#ver2[@]}; i++))
do
    ver1[i]=0
done
# starting from major of ver1
for ((i=0; i<${#ver1[@]}; i++))
do
    if [[ -z ${ver2[i]} ]]
    then
        # if ver2 shorter ver1 then
        # fill absent fields in ver2 with zeros
        ver2[i]=0
    fi
    if ((10#${ver1[i]} > 10#${ver2[i]}))
    then
        # if ver1 greater than ver2 in most major differing field
        echo 1
        exit
    fi
    if ((10#${ver1[i]} < 10#${ver2[i]}))
    then
        # if ver2 greater than ver1 in most major differing field
        echo 2
        exit
    fi
done
echo 0
exit