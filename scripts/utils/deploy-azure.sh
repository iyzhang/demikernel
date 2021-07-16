#!/bin/bash

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

TARGET=$1
PREFIX="/home/ppenna"
DEMIKERNEL_PATH="$PREFIX/demikernel/demikernel"

./scripts/utils/deploy.sh demikernel0 "$DEMIKERNEL_PATH" "$TARGET" &
./scripts/utils/deploy.sh demikernel1 "$DEMIKERNEL_PATH" "$TARGET" &

wait $(jobs -p)
