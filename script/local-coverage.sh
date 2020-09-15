#!/bin/bash

grcov ./target/debug \
  --output-type lcov \
  --llvm \
  --branch \
  --ignore-not-existing \
  --source-dir . \
  --output-path target/lcov.info \
  --excl-start '// EXCL_COVERAGE_START'\
  --excl-stop '// EXCL_COVERAGE_STOP'

grcov ./target/debug \
  --output-type html \
  --llvm \
  --branch \
  --ignore-not-existing \
  --source-dir . \
  --output-path target/coverage \
  --excl-start '// EXCL_COVERAGE_START'\
  --excl-stop '// EXCL_COVERAGE_STOP'
