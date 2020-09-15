#!/bin/bash

grcov ./target/debug \
  --output-type lcov \
  --llvm \
  --branch \
  --ignore-not-existing \
  --source-dir . \
  --output-path target/lcov.info \
  --excl-start '// EXCL_COVERAGE_START'\
  --excl-stop '// EXCL_COVERAGE_STOP' \
  --token $CODECOV_TOKEN

bash <(curl -s https://codecov.io/bash) -f target/lcov.info
