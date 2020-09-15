#!/bin/bash

grcov ./target/debug \
  --output-type lcov \
  --llvm \
  --branch \
  --ignore-not-existing \
  --source-dir . \
  --output-path target/lcov.info \
  --excl-start '// LCOV_EXCL_START'\
  --excl-stop '// LCOV_EXCL_END'

grcov ./target/debug \
  --output-type html \
  --llvm \
  --branch \
  --ignore-not-existing \
  --source-dir . \
  --output-path target/coverage \
  --excl-start '// LCOV_EXCL_START'\
  --excl-stop '// LCOV_EXCL_END'
