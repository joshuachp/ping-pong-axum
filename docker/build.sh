#!/usr/bin/env bash

set -exEuo pipefail

docker build -t sender -f docker/sender/Dockerfile .
docker build -t receiver -f docker/receiver/Dockerfile .
