set -eu
od -An -N16 -tx1 /dev/urandom | tr -d " \n" > .seed
