#!/bin/bash

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

REMOTE=$1
REMOTE_PATH=$2
TARGET=$3

set -e

EXCLUDE_LIST="--exclude=target --exclude=.git --exclude=*.swp"

ssh $REMOTE "bash -l -c 'mkdir -p $REMOTE_PATH'"
rsync -avz --delete-after $EXCLUDE_LIST . "$REMOTE":"$REMOTE_PATH"
ssh $REMOTE "bash -l -c 'cd $REMOTE_PATH && make $TARGET'"
