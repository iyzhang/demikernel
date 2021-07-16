#!/bin/bash

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

REMOTE=$1
ARGS=$2
BIN=$3
CONFIG=$4

RUNCMD="$ARGS $BIN $CONFIG"

ssh $REMOTE "bash -l -c 'sudo $RUNCMD'"

