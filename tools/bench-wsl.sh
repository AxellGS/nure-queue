#!/bin/bash
export PATH="$HOME/.cargo/bin:$PATH"
if [ "$1" = "bg" ]; then
  nohup bash "$0" run > /tmp/nure-bench-launch.log 2>&1 &
  echo "launched pid $!"
  exit 0
fi
cd ~/nure-queue || exit 1
rustc --version
nproc
cargo bench -- spsc_100k_fifo
cargo bench -- mpmc_4p4c_100k
echo BENCH_DONE
