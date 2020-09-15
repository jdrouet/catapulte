#!/bin/bash

grcov ./target/debug \
  --output-type lcov \
  --llvm \
  --branch \
  --ignore-not-existing \
  --source-dir . \
  --output-path target/lcov.info \
  --excl-start '// LCOV_EXCL_START'\
  --excl-stop '// LCOV_EXCL_END' \
  --token $CODECOV_TOKEN

bash <(curl -s https://codecov.io/bash) -f target/lcov.info
