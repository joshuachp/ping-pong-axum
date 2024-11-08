#!/usr/bin/env sh

set -exu

exec sender --address="0.0.0.0" "$@"
