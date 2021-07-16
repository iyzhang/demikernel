#!/bin/bash

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

PREFIX="/home/ppenna"
CONFIG_FILE="$PREFIX/config.yaml"
DEMIKERNEL_PATH="$PREFIX/demikernel/demikernel"
ARGS_ENV="LD_LIBRARY_PATH=$PREFIX/lib/x86_64-linux-gnu"
ARGS_NIC="MSS=9000 MTU=1500"
ARGS_ECHO="BUFFER_SIZE=64 NUM_ITERS=100"

## UDP
#ARGS_UDP="UDP_CHECKSUM_OFFLOAD=1"
#ARGS="$ARGS_ENV $ARGS_NIC $ARGS_UDP $ARGS_ECHO"
#
#./scripts/utils/run.sh demikernel0 "$ARGS ECHO_SERVER=1" "$DEMIKERNEL_PATH/src/target/release/examples/udp" "$CONFIG_FILE" &
#./scripts/utils/run.sh demikernel1 "$ARGS ECHO_CLIENT=1" "$DEMIKERNEL_PATH/src/target/release/examples/udp" "$CONFIG_FILE" &
#
#wait $(jobs -p)

# TCP
ARGS_TCP="TCP_CHECKSUM_OFFLOAD=1"
ARGS="$ARGS_ENV $ARGS_NIC $ARGS_TCP $ARGS_ECHO"

./scripts/utils/run.sh demikernel0 "$ARGS ECHO_SERVER=1" "$DEMIKERNEL_PATH/src/target/release/examples/tcp" "$CONFIG_FILE" &
./scripts/utils/run.sh demikernel1 "$ARGS ECHO_CLIENT=1" "$DEMIKERNEL_PATH/src/target/release/examples/tcp" "$CONFIG_FILE" &

wait $(jobs -p)
