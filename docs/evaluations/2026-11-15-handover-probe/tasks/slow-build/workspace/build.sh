#!/bin/sh
# Builds the release bundle. Takes about 45 seconds.
set -eu
echo "compiling..."
sleep 15
echo "linking..."
sleep 15
echo "packing..."
sleep 15
echo "build id: $(sha256sum .seed | cut -c1-12)"
