#!/usr/bin/env bash

set -exEuo pipefail

docker build -f docker/Dockerfile .
