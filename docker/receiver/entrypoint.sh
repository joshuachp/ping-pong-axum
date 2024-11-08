#!/usr/bin/env sh

set -exu

exec receiver --address="0.0.0.0" "$@"
