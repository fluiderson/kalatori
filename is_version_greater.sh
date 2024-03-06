#!/bin/bash
IFS=.
version=($1)
before_version=($2)
# starting from minor of version if version shorter before_version
# fill absent fields in version with zeros
for ((i=${#version[@]}; i<${#before_version[@]}; i++))
do
    version[i]=0
done
# starting from major of version
for ((i=0; i<${#version[@]}; i++))
do
    if [[ -z ${before_version[i]} ]]
    then
        # if before_version shorter version then
        # fill absent fields in before_version with zeros
        ver2[i]=0
    fi
    if ((10#${version[i]} > 10#${before_version[i]}))
    then
        # if version greater than before_version in most major differing field
        exit 0
    fi
    if ((10#${version[i]} < 10#${before_version[i]}))
    then
        # if version is not greater in most major differing field
        exit 1
    fi
done
exit 1