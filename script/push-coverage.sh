#!/bin/bash

bash <(curl -s https://codecov.io/bash) -f target/lcov.info
