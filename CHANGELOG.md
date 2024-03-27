# Changelog

All notable changes to this project will be documented in this file.

## [1.1.0](https://github.com/jdrouet/catapulte/compare/catapulte-v1.0.0...catapulte-v1.1.0) - 2024-03-27

### Added
- integrate catapulte engine
- *(engine)* add memory loader
- *(engine)* init lib
- allow to specify config path through env variable
- *(serve)* add opportunity to have trace id logged for each request
- *(serve)* add graceful shutdown
- allow to disable color in logs
- *(provider)* add some tests
- *(provider)* create a new http provider
- *(provider)* update local provider
- *(openapi)* create a command to print the openapi json schema
- *(axum)* remove useless deps
- *(axum)* add more tests
- *(axum)* implement more tests for json handler
- *(axum)* remove unused jolimail provider
- *(axum)* update documentation
- *(axum)* delete swagger folder
- *(axum)* update errors and tests
- *(axum)* update openapi definitions
- *(axum)* update dockerfile to add healthcheck
- *(axum)* add some logs and metrics
- *(axum)* generate openapi
- *(axum)* simplify application with axum
- *(server)* add cli options ([#181](https://github.com/jdrouet/catapulte/pull/181))
- *(smtp)* fix tests
- *(server)* run tests in docker-compose
- *(smtp)* ensure invalid tls is working
- *(smtp)* allow to connect with invalid cert
- *(server)* create authentication middleware
- *(server)* add authentication service
- *(server)* update openapi to add cc, bcc and attachments
- *(server)* hide swagger behing environment variable
- *(server)* add controller for swagger
- *(server)* add openapi into swagger folder
- add alpine dockerfile
- *(rustls)* replace native-tls by rustls
- can set multiple emails in to, cc and bcc
- *(smtp)* handle tls connection
- *(heroku)* use container stack and define required files
- *(templates)* can send local template
- *(smtp)* create connection to smtp

### Fixed
- *(docker)* add missing lib folder
- update cli typo
- Dockerfile to reduce vulnerabilities
- remove double trace layer
- Dockerfile to reduce vulnerabilities
- Dockerfile to reduce vulnerabilities
- remove unused enum item
- *(build)* remove swagger from dockerfiles
- *(test)* update TEMPLATE_PATH in compose file
- *(ci)* use concurrency to cancel running jobs
- *(ci)* update dockerfile to make tests run
- *(ci)* force using buildkit
- *(ci)* install buildx to use docker-compose
- *(ci)* only build for amd64
- *(ci)* specify buildx platforms
- *(ci)* rename dockerfiles
- *(ci)* use good name for dockerfile
- *(ci)* use list of string for tags
- alpine.Dockerfile to reduce vulnerabilities ([#297](https://github.com/jdrouet/catapulte/pull/297))
- multiarch-alpine.Dockerfile to reduce vulnerabilities ([#165](https://github.com/jdrouet/catapulte/pull/165))
- *(ci)* use network mode to host when building images
- *(ci)* replace repository url to avoid hang up
- *(ci)* replace repository url to avoid hang up
- *(ci)* upgrade buildx and docker dind
- *(ci)* avoid hanging on apk add ([#236](https://github.com/jdrouet/catapulte/pull/236))
- *(server)* stop returning 404 where swagger enabled
- *(ci)* apply clippy's proposals
- *(ci)* allow coverage to fail
- *(server)* make status endpoint visible
- *(server)* add Bearer in the authentication header
- *(server)* fix arm64 build with docker
- reorder struct attributes
- *(docker)* remove Cargo.lock from dockerignore
- *(lint)* please clippy
- please clippy
- *(build)* pin version to allow build in docker
- apply clippy suggestions
- *(test)* replace localhost by 127.0.0.1 for local tests
- *(multipart)* use alternative method
- *(variable)* update docs around environment variables
- *(test)* remove cargo cache hanging
- prefix requests to jolimail with /api

### Other
- *(deps)* Bump serde_json from 1.0.114 to 1.0.115
- *(deps)* Bump clap from 4.5.3 to 4.5.4
- *(deps)* Bump handlebars from 5.1.0 to 5.1.2
- *(deps)* Bump reqwest from 0.12.1 to 0.12.2
- *(deps)* Bump axum from 0.7.4 to 0.7.5
- *(deps)* Bump bytes from 1.5.0 to 1.6.0
- *(deps)* Bump reqwest from 0.12.0 to 0.12.1
- updater dockerfiles
- use cargo wizard to optimise the execution
- *(deps)* Bump reqwest from 0.11.26 to 0.12.0
- *(deps)* Bump mrml from 3.1.0 to 3.1.3
- *(engine)* remove dependency on openssl-sys
- apply clippy changes
- update error handling at server level
- *(deps)* Bump uuid from 1.7.0 to 1.8.0
- *(deps)* Bump many dependencies
- update common workflow trigger
- remove clones
- *(deps)* Bump mrml from 3.0.4 to 3.1.0
- *(deps)* Bump clap from 4.5.2 to 4.5.3
- *(deps)* Bump reqwest from 0.11.25 to 0.11.26
- *(deps)* Bump thiserror from 1.0.57 to 1.0.58
- move arc to services
- refactor template provider
- use thiserror
- move test module to smtp
- move http server to service
- set packages versions
- update github actions
- *(deps)* Bump reqwest from 0.11.24 to 0.11.25
- *(deps)* Bump clap from 4.5.1 to 4.5.2
- release
- *(deps)* Bump mio from 0.8.10 to 0.8.11
- *(deps)* Bump mrml from 3.0.3 to 3.0.4
- *(deps)* Bump mrml from 3.0.2 to 3.0.3
- *(deps)* Bump mrml from 3.0.1 to 3.0.2
- *(deps)* Bump tempfile from 3.10.0 to 3.10.1
- *(deps)* Bump tower-http from 0.5.1 to 0.5.2
- *(deps)* Bump serde from 1.0.196 to 1.0.197
- *(deps)* Bump serde_json from 1.0.113 to 1.0.114
- *(deps)* Bump clap from 4.5.0 to 4.5.1
- *(deps)* Bump mrml from 3.0.0 to 3.0.1
- *(deps)* Bump metrics-exporter-prometheus from 0.13.0 to 0.13.1
- *(deps)* Bump metrics from 0.22.0 to 0.22.1
- *(deps)* Bump wiremock from 0.5.22 to 0.6.0
- *(deps)* Bump clap from 4.4.18 to 4.5.0
- *(deps)* Bump tempfile from 3.9.0 to 3.10.0
- *(deps)* Bump tokio from 1.35.1 to 1.36.0
- *(deps)* Bump config from 0.13.4 to 0.14.0
- *(deps)* Bump reqwest from 0.11.23 to 0.11.24
- *(deps)* Bump lettre from 0.11.3 to 0.11.4
- *(deps)* Bump serde_json from 1.0.112 to 1.0.113
- *(deps)* Bump serde from 1.0.195 to 1.0.196
- *(deps)* Bump serde_json from 1.0.111 to 1.0.112
- *(deps)* Bump h2 from 0.3.22 to 0.3.24
- *(deps)* Bump uuid from 1.6.1 to 1.7.0
- *(deps)* Bump handlebars from 5.1.0 to 5.1.1
- *(deps)* Bump handlebars from 5.0.0 to 5.1.0
- *(deps)* Bump clap from 4.4.17 to 4.4.18
- *(deps)* Bump axum from 0.7.3 to 0.7.4
- *(deps)* Bump tower-http from 0.5.0 to 0.5.1
- *(deps)* Bump clap from 4.4.16 to 4.4.17
- *(deps)* Bump clap from 4.4.15 to 4.4.16
- *(deps)* Bump clap from 4.4.14 to 4.4.15
- *(deps)* Bump utoipa-swagger-ui from 5.0.0 to 6.0.0
- *(deps)* Bump utoipa from 4.1.0 to 4.2.0
- *(deps)* Bump serde from 1.0.194 to 1.0.195
- *(deps)* Bump clap from 4.4.13 to 4.4.14
- *(deps)* Bump clap from 4.4.12 to 4.4.13
- Update CHANGELOG.md
- Update Cargo.toml
- release
- apply lint with stable rust version
- *(deps)* Bump mrml
- *(deps)* Bump handlerbars
- *(deps)* Bump metrics and related
- *(deps)* Bump axum and related
- *(deps)* Bump zerocopy from 0.7.28 to 0.7.31
- *(deps)* Bump tokio from 1.34.0 to 1.35.0
- *(deps)* bump deps
- update release-plz config
- update release-plz config
- release
- add example catapulte.toml file
- *(deps)* bump mrml to 2.0
- *(deps)* bump lettre to 0.11
- *(deps)* Bump wiremock from 0.5.21 to 0.5.22
- *(deps)* Bump clap from 4.4.9 to 4.4.10
- *(deps)* fully remove hyper
- *(deps)* Bump hyper from 0.14.27 to 1.0.1
- *(deps)* Bump clap from 4.4.8 to 4.4.9
- *(deps)* Bump config from 0.13.3 to 0.13.4
- *(deps)* Bump serde from 1.0.192 to 1.0.193
- update funding
- rename codebench config file
- change ci-metrics for alpine image
- move from codebench to ci-metrics
- *(deps)* Bump uuid from 1.5.0 to 1.6.1
- *(deps)* Bump utoipa from 4.0.0 to 4.1.0
- *(deps)* Bump tracing-subscriber from 0.3.17 to 0.3.18
- *(deps)* Bump handlebars from 4.4.0 to 4.5.0
- *(deps)* Bump tokio from 1.33.0 to 1.34.0
- *(deps)* Bump clap from 4.4.7 to 4.4.8
- *(deps)* Bump serde from 1.0.191 to 1.0.192
- *(deps)* Bump serde from 1.0.190 to 1.0.191
- *(deps)* Bump wiremock from 0.5.19 to 0.5.21
- *(ci)* update events units
- *(ci)* update command to push metrics
- *(deps)* Bump serde_json from 1.0.107 to 1.0.108
- *(deps)* Bump serde from 1.0.189 to 1.0.190
- *(deps)* Bump tempfile from 3.8.0 to 3.8.1
- *(deps)* Bump clap from 4.4.6 to 4.4.7
- *(deps)* Bump tracing from 0.1.39 to 0.1.40
- *(deps)* Bump rustix from 0.38.17 to 0.38.19
- *(deps)* Bump uuid from 1.4.1 to 1.5.0
- *(deps)* Bump serde from 1.0.188 to 1.0.189
- *(deps)* Bump tracing from 0.1.37 to 0.1.39
- *(deps)* Bump utoipa and utoipa-swagger-ui to 4.0
- *(deps)* Bump tokio from 1.32.0 to 1.33.0
- *(ci)* add missing fetch-depth
- *(ci)* automagically create pr
- *(deps)* update dependencies
- *(deps)* Bump reqwest from 0.11.21 to 0.11.22
- *(deps)* Bump reqwest from 0.11.20 to 0.11.21
- *(ci)* publish docker image size
- *(deps)* Bump clap from 4.4.5 to 4.4.6
- *(deps)* Bump clap from 4.4.4 to 4.4.5
- *(deps)* Bump clap from 4.4.3 to 4.4.4
- *(deps)* Bump serde_json from 1.0.106 to 1.0.107
- *(deps)* Bump clap from 4.4.2 to 4.4.3
- *(deps)* Bump serde_json from 1.0.105 to 1.0.106
- *(deps)* Bump handlebars from 4.3.7 to 4.4.0
- *(deps)* Bump tower-http from 0.4.3 to 0.4.4
- *(deps)* Bump clap from 4.4.1 to 4.4.2
- *(deps)* Bump serde from 1.0.187 to 1.0.188
- *(deps)* Bump clap from 4.4.0 to 4.4.1
- *(deps)* Bump serde from 1.0.186 to 1.0.187
- *(deps)* Bump serde from 1.0.185 to 1.0.186
- *(deps)* Bump clap from 4.3.24 to 4.4.0
- *(deps)* Bump reqwest from 0.11.19 to 0.11.20
- *(deps)* Bump clap from 4.3.23 to 4.3.24
- *(deps)* Bump rustls-webpki from 0.100.1 to 0.100.2
- *(deps)* Bump utoipa from 3.4.4 to 3.5.0
- *(deps)* Bump reqwest from 0.11.18 to 0.11.19
- *(deps)* Bump serde from 1.0.183 to 1.0.185
- *(deps)* Bump tempfile from 3.7.1 to 3.8.0
- *(deps)* Bump clap from 4.3.22 to 4.3.23
- *(deps)* Bump lettre to 0.10
- *(deps)* Bump tokio from 1.31.0 to 1.32.0
- *(docker)* bump debian tag from bullseye to bookworm
- *(deps)* run cargo update to bump deps
- *(deps)* Bump serde_json from 1.0.104 to 1.0.105
- *(deps)* Bump tokio from 1.30.0 to 1.31.0
- *(deps)* Bump tokio from 1.29.1 to 1.30.0
- *(deps)* Bump clap from 4.3.19 to 4.3.21
- *(deps)* Bump utoipa-swagger-ui from 3.1.4 to 3.1.5
- *(deps)* Bump tempfile from 3.7.0 to 3.7.1
- *(deps)* Bump serde from 1.0.181 to 1.0.183
- *(deps)* Bump utoipa from 3.4.3 to 3.4.4
- *(deps)* Bump serde from 1.0.180 to 1.0.181
- *(deps)* Bump axum from 0.6.19 to 0.6.20
- *(deps)* Bump serde from 1.0.178 to 1.0.180
- *(deps)* Bump serde from 1.0.177 to 1.0.178
- *(deps)* Bump serde from 1.0.176 to 1.0.177
- *(deps)* Bump serde_json from 1.0.103 to 1.0.104
- *(deps)* Bump serde from 1.0.175 to 1.0.176
- *(deps)* Bump serde from 1.0.174 to 1.0.175
- *(deps)* Bump utoipa from 3.4.2 to 3.4.3
- *(deps)* Bump utoipa from 3.4.0 to 3.4.2
- *(deps)* Bump clap from 4.3.17 to 4.3.19
- *(deps)* Bump serde from 1.0.171 to 1.0.174
- *(deps)* Bump clap from 4.3.16 to 4.3.17
- *(deps)* Bump tower-http from 0.4.2 to 0.4.3
- *(deps)* Bump tempfile from 3.6.0 to 3.7.0
- *(deps)* Bump tower-http from 0.4.1 to 0.4.2
- *(deps)* Bump serde_json from 1.0.102 to 1.0.103
- *(deps)* Bump hyper from 0.14.26 to 0.14.27
- *(deps)* Bump clap from 4.3.12 to 4.3.16
- *(deps)* Bump uuid from 1.4.0 to 1.4.1
- *(deps)* Bump axum from 0.6.18 to 0.6.19
- *(deps)* Bump hyper from 0.14.24 to 0.14.26
- *(deps)* Bump clap from 4.3.11 to 4.3.12
- *(deps)* Bump utoipa-swagger-ui from 3.1.3 to 3.1.4
- *(deps)* Bump utoipa from 3.3.0 to 3.4.0
- *(deps)* Bump serde_json from 1.0.101 to 1.0.102
- *(deps)* Bump serde_json from 1.0.100 to 1.0.101
- *(deps)* Bump serde from 1.0.167 to 1.0.171
- *(deps)* Bump serde from 1.0.166 to 1.0.167
- *(deps)* Bump clap from 4.3.10 to 4.3.11
- *(deps)* Bump clap from 4.1.11 to 4.3.10
- *(deps)* Bump metrics from 0.21.0 to 0.21.1
- *(deps)* Bump serde_json from 1.0.99 to 1.0.100
- *(deps)* Bump tokio from 1.29.0 to 1.29.1
- *(deps)* Bump tokio from 1.28.2 to 1.29.0
- *(deps)* Bump uuid from 1.3.4 to 1.4.0
- *(deps)* Bump serde_json from 1.0.97 to 1.0.99
- *(deps)* Bump tower-http from 0.4.0 to 0.4.1
- *(deps)* Bump serde_json from 1.0.96 to 1.0.97
- *(deps)* Bump uuid from 1.3.3 to 1.3.4
- *(deps)* Bump serde from 1.0.163 to 1.0.164
- *(deps)* Bump wiremock from 0.5.18 to 0.5.19
- *(deps)* Bump tempfile from 3.5.0 to 3.6.0
- *(deps)* Bump tokio from 1.28.1 to 1.28.2
- *(deps)* Bump reqwest from 0.11.17 to 0.11.18
- *(deps)* Bump uuid from 1.3.2 to 1.3.3
- *(deps)* Bump serde from 1.0.162 to 1.0.163
- *(deps)* Bump handlebars from 4.3.6 to 4.3.7
- *(deps)* Bump tokio from 1.28.0 to 1.28.1
- *(deps)* Bump metrics-exporter-prometheus from 0.12.0 to 0.12.1
- *(deps)* Bump serde from 1.0.161 to 1.0.162
- *(deps)* Bump serde from 1.0.160 to 1.0.161
- *(deps)* Bump tokio from 1.27.0 to 1.28.0
- *(deps)* Bump reqwest from 0.11.16 to 0.11.17
- *(deps)* Bump axum from 0.6.16 to 0.6.18
- *(deps)* Bump uuid from 1.3.1 to 1.3.2
- *(deps)* Bump tracing from 0.1.37 to 0.1.38
- *(deps)* Bump tracing-subscriber from 0.3.16 to 0.3.17
- *(deps)* Bump axum from 0.6.15 to 0.6.16
- *(deps)* Bump metrics from 0.20.1 to 0.21.0
- *(deps)* Bump metrics-exporter-prometheus from 0.11.0 to 0.12.0
- *(deps)* Bump utoipa from 3.2.1 to 3.3.0
- *(deps)* Bump h2 from 0.3.15 to 0.3.17
- *(deps)* Bump axum from 0.6.14 to 0.6.15
- *(deps)* Bump serde_json from 1.0.95 to 1.0.96
- *(deps)* Bump axum from 0.6.12 to 0.6.14
- *(deps)* Bump serde from 1.0.159 to 1.0.160
- *(deps)* Bump uuid from 1.3.0 to 1.3.1
- *(deps)* Bump utoipa-swagger-ui from 3.1.2 to 3.1.3
- *(deps)* Bump wiremock from 0.5.17 to 0.5.18
- *(deps)* Bump utoipa-swagger-ui from 3.1.1 to 3.1.2
- *(deps)* Bump utoipa from 3.2.0 to 3.2.1
- add some logs for creating smtp service
- *(deps)* Bump serde from 1.0.158 to 1.0.159
- *(deps)* Bump tempfile from 3.4.0 to 3.5.0
- *(deps)* Bump utoipa from 3.1.2 to 3.2.0
- *(deps)* Bump reqwest from 0.11.15 to 0.11.16
- *(deps)* Bump tokio from 1.26.0 to 1.27.0
- *(deps)* Bump serde_json from 1.0.94 to 1.0.95
- *(deps)* Bump axum from 0.6.11 to 0.6.12
- *(deps)* Bump serde from 1.0.156 to 1.0.158
- *(deps)* Bump reqwest from 0.11.14 to 0.11.15
- *(deps)* Bump clap from 4.1.9 to 4.1.11
- *(deps)* Bump utoipa from 3.1.1 to 3.1.2
- *(deps)* Bump clap from 4.1.8 to 4.1.9
- *(deps)* Bump utoipa from 3.1.0 to 3.1.1
- *(deps)* Bump utoipa-swagger-ui from 3.1.0 to 3.1.1
- *(deps)* Bump serde from 1.0.155 to 1.0.156
- *(deps)* Bump serde from 1.0.154 to 1.0.155
- *(deps)* Bump axum from 0.6.10 to 0.6.11
- *(deps)* Bump utoipa-swagger-ui from 3.0.2 to 3.1.0
- *(deps)* Bump utoipa from 3.0.3 to 3.1.0
- *(deps)* Bump serde from 1.0.153 to 1.0.154
- *(deps)* Bump serde from 1.0.152 to 1.0.153
- *(deps)* Bump serde_json from 1.0.93 to 1.0.94
- *(deps)* Bump axum from 0.6.9 to 0.6.10
- *(deps)* Bump tokio from 1.25.0 to 1.26.0
- *(deps)* Bump clap from 4.1.7 to 4.1.8
- *(deps)* Bump clap from 4.1.6 to 4.1.7
- *(deps)* Bump axum from 0.6.8 to 0.6.9
- *(deps)* Bump tempfile from 3.3.0 to 3.4.0
- *(deps)* Bump axum from 0.6.7 to 0.6.8
- *(deps)* Bump tower-http from 0.3.5 to 0.4.0
- *(deps)* Bump utoipa from 3.0.2 to 3.0.3
- *(deps)* Bump axum from 0.6.6 to 0.6.7
- *(deps)* Bump clap from 4.1.4 to 4.1.6
- *(deps)* Bump axum from 0.6.4 to 0.6.6
- *(deps)* Bump utoipa from 3.0.1 to 3.0.2
- *(deps)* Bump serde_json from 1.0.92 to 1.0.93
- *(deps)* Bump serde_json from 1.0.91 to 1.0.92
- *(deps)* Bump uuid from 1.2.2 to 1.3.0
- *(deps)* Bum utoipa and utoipa to 3.0
- *(deps)* Bump tokio from 1.24.2 to 1.25.0
- fix clippy suggestions
- *(deps)* Bump clap from 4.1.3 to 4.1.4
- *(deps)* Bump axum from 0.6.3 to 0.6.4
- Merge pull request [#404](https://github.com/jdrouet/catapulte/pull/404) from jdrouet/dependabot/cargo/clap-4.1.3
- *(deps)* Bump axum from 0.6.2 to 0.6.3
- *(deps)* Bump reqwest from 0.11.13 to 0.11.14
- *(deps)* Bump tokio from 1.24.1 to 1.24.2
- *(deps)* Bump clap from 4.1.0 to 4.1.1
- *(deps)* Bump clap from 4.0.32 to 4.1.0
- *(deps)* Bump wiremock from 0.5.16 to 0.5.17
- *(deps)* Bump mrml from 1.2.10 to 1.2.11
- *(deps)* Bump axum from 0.6.1 to 0.6.2
- *(deps)* Bump tokio from 1.24.0 to 1.24.1
- *(deps)* Bump tokio from 1.23.1 to 1.24.0
- *(deps)* Bump tokio from 1.23.0 to 1.23.1
- *(deps)* Bump wiremock from 0.5.15 to 0.5.16
- *(deps)* Bump serde from 1.0.151 to 1.0.152
- *(deps)* Bump clap from 4.0.30 to 4.0.32
- *(deps)* Bump clap from 4.0.29 to 4.0.30
- *(deps)* Bump handlebars from 4.3.5 to 4.3.6
- *(deps)* Bump serde_json from 1.0.89 to 1.0.91
- *(deps)* Bump serde from 1.0.150 to 1.0.151
- please clippy
- *(deps)* Bump serde from 1.0.149 to 1.0.150
- *(ci)* release canary and version when tagged
- *(ci)* use workflow file to trigger build
- *(ci)* make testing job go faster
- *(ci)* take binaries out of docker context
- *(ci)* install qemu
- *(ci)* update platforms
- *(ci)* specify version to trigger release manually
- *(ci)* add stage to build binaries
- *(ci)* make release build triggerable manually
- *(deps)* Bump tokio from 1.22.0 to 1.23.0
- *(deps)* Bump config from 0.13.2 to 0.13.3
- *(deps)* Bump serde from 1.0.148 to 1.0.149
- *(deps)* Bump tower-http from 0.3.4 to 0.3.5
- *(deps)* Bump clap from 4.0.26 to 4.0.27
- *(deps)* Bump serde_json from 1.0.87 to 1.0.89
- *(ci)* limit concurrency for main worflow
- *(ci)* prepare release workflow
- *(ci)* migrate to github actions
- *(deps)* Bump reqwest from 0.11.12 to 0.11.13
- *(deps)* Bump clap from 4.0.24 to 4.0.26
- *(deps)* Bump clap from 4.0.23 to 4.0.24
- *(deps)* Bump uuid from 1.2.1 to 1.2.2
- *(deps)* Bump clap from 4.0.22 to 4.0.23
- *(deps)* Bump env_logger from 0.9.1 to 0.9.3
- *(deps)* Bump clap from 4.0.18 to 4.0.22
- *(deps)* Bump serde from 1.0.145 to 1.0.147
- *(deps)* Bump clap from 4.0.17 to 4.0.18
- *(deps)* Bump futures from 0.3.24 to 0.3.25
- *(deps)* Bump serde_json from 1.0.86 to 1.0.87
- *(deps)* Bump async-trait from 0.1.57 to 0.1.58
- *(deps)* Bump clap from 4.0.15 to 4.0.17
- *(deps)* Bump clap from 4.0.14 to 4.0.15
- *(deps)* Bump clap from 4.0.10 to 4.0.14
- *(deps)* Bump wiremock from 0.5.14 to 0.5.15 ([#347](https://github.com/jdrouet/catapulte/pull/347))
- *(deps)* Bump uuid from 1.1.2 to 1.2.1 ([#349](https://github.com/jdrouet/catapulte/pull/349))
- *(deps)* Bump serde_json from 1.0.85 to 1.0.86 ([#350](https://github.com/jdrouet/catapulte/pull/350))
- *(deps)* upgrade actix deps ([#344](https://github.com/jdrouet/catapulte/pull/344))
- *(docker)* bump docker images to bulleye ([#343](https://github.com/jdrouet/catapulte/pull/343))
- *(deps)* bump alpine image version ([#342](https://github.com/jdrouet/catapulte/pull/342))
- *(deps)* Bump common-multipart-rfc7578 from 0.5.0 to 0.6.0
- *(deps)* bump serial_test to 0.9 ([#341](https://github.com/jdrouet/catapulte/pull/341))
- *(deps)* Bump clap to 4.0.10 ([#340](https://github.com/jdrouet/catapulte/pull/340))
- apply clippy suggestions ([#339](https://github.com/jdrouet/catapulte/pull/339))
- Fix invalid docker image tag ([#338](https://github.com/jdrouet/catapulte/pull/338))
- *(deps)* Bump lettre from 0.10.0-rc.5 to 0.10.0-rc.6 ([#312](https://github.com/jdrouet/catapulte/pull/312))
- *(deps)* Bump clap from 3.1.10 to 3.1.12 ([#311](https://github.com/jdrouet/catapulte/pull/311))
- *(deps)* Bump uuid from 0.8.2 to 1.0.0 ([#310](https://github.com/jdrouet/catapulte/pull/310))
- *(deps)* Bump wiremock from 0.5.12 to 0.5.13 ([#308](https://github.com/jdrouet/catapulte/pull/308))
- *(deps)* Bump clap from 3.1.9 to 3.1.10 ([#309](https://github.com/jdrouet/catapulte/pull/309))
- *(deps)* Bump common-multipart-rfc7578 from 0.3.1 to 0.5.0 ([#296](https://github.com/jdrouet/catapulte/pull/296))
- *(deps)* Bump wiremock from 0.5.11 to 0.5.12 ([#305](https://github.com/jdrouet/catapulte/pull/305))
- *(deps)* Bump clap from 3.1.6 to 3.1.9 ([#307](https://github.com/jdrouet/catapulte/pull/307))
- *(deps)* Bump lettre from 0.10.0-rc.4 to 0.10.0-rc.5 ([#304](https://github.com/jdrouet/catapulte/pull/304))
- *(deps)* Bump jsonwebtoken from 8.0.1 to 8.1.0 ([#306](https://github.com/jdrouet/catapulte/pull/306))
- Merge pull request [#300](https://github.com/jdrouet/catapulte/pull/300) from jdrouet/dependabot/cargo/mrml-1.2.10
- *(deps)* Bump async-trait from 0.1.52 to 0.1.53
- *(deps)* Bump mrml from 1.2.8 to 1.2.9 ([#298](https://github.com/jdrouet/catapulte/pull/298))
- *(deps)* Bump log from 0.4.14 to 0.4.16 ([#299](https://github.com/jdrouet/catapulte/pull/299))
- *(deps)* Bump reqwest from 0.11.9 to 0.11.10 ([#295](https://github.com/jdrouet/catapulte/pull/295))
- *(deps)* Bump mrml from 1.2.7 to 1.2.8 ([#294](https://github.com/jdrouet/catapulte/pull/294))
- *(deps)* Bump handlebars from 4.2.1 to 4.2.2 ([#292](https://github.com/jdrouet/catapulte/pull/292))
- *(deps)* Bump actix-http from 3.0.3 to 3.0.4 ([#291](https://github.com/jdrouet/catapulte/pull/291))
- *(deps)* Bump actix-rt from 2.6.0 to 2.7.0 ([#290](https://github.com/jdrouet/catapulte/pull/290))
- *(deps)* Bump actix-http from 3.0.2 to 3.0.3 ([#289](https://github.com/jdrouet/catapulte/pull/289))
- *(deps)* Bump clap from 3.1.5 to 3.1.6 ([#287](https://github.com/jdrouet/catapulte/pull/287))
- *(deps)* Bump actix-http from 3.0.1 to 3.0.2 ([#288](https://github.com/jdrouet/catapulte/pull/288))
- *(deps)* Bump actix-http from 3.0.0 to 3.0.1 ([#286](https://github.com/jdrouet/catapulte/pull/286))
- *(deps)* Bump clap from 3.1.3 to 3.1.5 ([#284](https://github.com/jdrouet/catapulte/pull/284))
- *(deps)* Bump wiremock from 0.5.10 to 0.5.11 ([#282](https://github.com/jdrouet/catapulte/pull/282))
- *(deps)* Bump clap from 3.1.2 to 3.1.3 ([#283](https://github.com/jdrouet/catapulte/pull/283))
- *(deps)* Bump actix-http from 3.0.0-rc.4 to 3.0.0 ([#281](https://github.com/jdrouet/catapulte/pull/281))
- *(deps)* Bump actix-http from 3.0.0-rc.3 to 3.0.0-rc.4 ([#279](https://github.com/jdrouet/catapulte/pull/279))
- *(deps)* Bump clap from 3.1.0 to 3.1.2 ([#280](https://github.com/jdrouet/catapulte/pull/280))
- *(deps)* Bump serial_test from 0.5.1 to 0.6.0 ([#278](https://github.com/jdrouet/catapulte/pull/278))
- *(deps)* Bump actix-http from 3.0.0-rc.2 to 3.0.0-rc.3 ([#276](https://github.com/jdrouet/catapulte/pull/276))
- *(deps)* Bump clap from 3.0.14 to 3.1.0 ([#275](https://github.com/jdrouet/catapulte/pull/275))
- *(deps)* Bump serde_json from 1.0.78 to 1.0.79 ([#274](https://github.com/jdrouet/catapulte/pull/274))
- *(deps)* Bump actix-web from 4.0.0-rc.2 to 4.0.0-rc.3 ([#273](https://github.com/jdrouet/catapulte/pull/273))
- *(deps)* Bump actix-http from 3.0.0-rc.1 to 3.0.0-rc.2
- *(deps)* Bump futures from 0.3.19 to 0.3.21 ([#270](https://github.com/jdrouet/catapulte/pull/270))
- *(deps)* Bump jsonwebtoken from 8.0.0 to 8.0.1
- *(deps)* Bump jsonwebtoken from 7.2.0 to 8.0.0 ([#268](https://github.com/jdrouet/catapulte/pull/268))
- *(deps)* Bump actix-http from 3.0.0-beta.19 to 3.0.0-rc.1 ([#266](https://github.com/jdrouet/catapulte/pull/266))
- *(deps)* Bump clap from 3.0.13 to 3.0.14 ([#267](https://github.com/jdrouet/catapulte/pull/267))
- *(deps)* Bump mrml from 1.2.6 to 1.2.7 ([#265](https://github.com/jdrouet/catapulte/pull/265))
- *(deps)* Bump clap from 3.0.12 to 3.0.13 ([#264](https://github.com/jdrouet/catapulte/pull/264))
- *(deps)* Bump serde from 1.0.135 to 1.0.136 ([#263](https://github.com/jdrouet/catapulte/pull/263))
- *(deps)* Bump serde_json from 1.0.75 to 1.0.78 ([#259](https://github.com/jdrouet/catapulte/pull/259))
- *(deps)* Bump serde from 1.0.134 to 1.0.135 ([#260](https://github.com/jdrouet/catapulte/pull/260))
- *(deps)* Bump clap from 3.0.10 to 3.0.12 ([#261](https://github.com/jdrouet/catapulte/pull/261))
- *(deps)* Bump actix-web from 4.0.0-beta.20 to 4.0.0-beta.21 ([#262](https://github.com/jdrouet/catapulte/pull/262))
- Merge pull request [#258](https://github.com/jdrouet/catapulte/pull/258) from jdrouet/dependabot/cargo/actix-http-3.0.0-beta.19
- *(deps)* Bump actix-http from 3.0.0-beta.18 to 3.0.0-beta.19
- *(deps)* Bump clap from 3.0.8 to 3.0.10 ([#256](https://github.com/jdrouet/catapulte/pull/256))
- *(deps)* Bump serde_json from 1.0.74 to 1.0.75 ([#254](https://github.com/jdrouet/catapulte/pull/254))
- *(deps)* Bump handlebars from 4.2.0 to 4.2.1 ([#255](https://github.com/jdrouet/catapulte/pull/255))
- *(deps)* Bump clap from 3.0.7 to 3.0.8 ([#253](https://github.com/jdrouet/catapulte/pull/253))
- *(deps)* Bump actix-web from 4.0.0-beta.19 to 4.0.0-beta.20
- *(deps)* Bump actix-rt from 2.5.1 to 2.6.0 ([#251](https://github.com/jdrouet/catapulte/pull/251))
- *(deps)* Bump clap from 3.0.6 to 3.0.7 ([#250](https://github.com/jdrouet/catapulte/pull/250))
- *(deps)* Bump reqwest from 0.11.8 to 0.11.9 ([#248](https://github.com/jdrouet/catapulte/pull/248))
- *(deps)* Bump clap from 3.0.5 to 3.0.6 ([#249](https://github.com/jdrouet/catapulte/pull/249))
- *(deps)* Bump wiremock from 0.5.9 to 0.5.10 ([#247](https://github.com/jdrouet/catapulte/pull/247))
- *(deps)* Bump actix-web, actix-http and actix-multipart ([#246](https://github.com/jdrouet/catapulte/pull/246))
- *(deps)* Bump clap from 3.0.4 to 3.0.5 ([#245](https://github.com/jdrouet/catapulte/pull/245))
- *(deps)* Bump handlebars from 4.1.6 to 4.2.0 ([#244](https://github.com/jdrouet/catapulte/pull/244))
- *(deps)* Bump wiremock from 0.5.8 to 0.5.9 ([#243](https://github.com/jdrouet/catapulte/pull/243))
- *(deps)* Bump serde_json from 1.0.73 to 1.0.74 ([#238](https://github.com/jdrouet/catapulte/pull/238))
- *(deps)* Bump clap from 3.0.1 to 3.0.4 ([#240](https://github.com/jdrouet/catapulte/pull/240))
- *(deps)* Bump serde from 1.0.132 to 1.0.133 ([#237](https://github.com/jdrouet/catapulte/pull/237))
- *(deps)* Bump clap from 3.0.0 to 3.0.1 ([#239](https://github.com/jdrouet/catapulte/pull/239))
- *(deps)* Bump clap from 3.0.0-beta.5 to 3.0.0 ([#235](https://github.com/jdrouet/catapulte/pull/235))
- *(deps)* Bump actix-rt from 2.5.0 to 2.5.1 ([#234](https://github.com/jdrouet/catapulte/pull/234))
- *(deps)* Bump futures from 0.3.17 to 0.3.19 ([#227](https://github.com/jdrouet/catapulte/pull/227))
- *(deps)* Bump reqwest from 0.11.7 to 0.11.8 ([#226](https://github.com/jdrouet/catapulte/pull/226))
- *(deps)* Bump lettre to 0.10.0-rc4 ([#223](https://github.com/jdrouet/catapulte/pull/223))
- *(deps)* Bump actix packages ([#220](https://github.com/jdrouet/catapulte/pull/220))
- *(deps)* Bump serde from 1.0.130 to 1.0.131 ([#212](https://github.com/jdrouet/catapulte/pull/212))
- *(deps)* Bump serde_json from 1.0.72 to 1.0.73 ([#217](https://github.com/jdrouet/catapulte/pull/217))
- *(deps)* Bump async-trait from 0.1.51 to 0.1.52 ([#211](https://github.com/jdrouet/catapulte/pull/211))
- *(deps)* Bump handlebars from 4.1.5 to 4.1.6
- *(deps)* Bump reqwest from 0.11.6 to 0.11.7 ([#206](https://github.com/jdrouet/catapulte/pull/206))
- *(deps)* Bump actix-web from 4.0.0-beta.12 to 4.0.0-beta.13 ([#205](https://github.com/jdrouet/catapulte/pull/205))
- *(deps)* Bump funty from 1.1.0 to 2.0.0 ([#204](https://github.com/jdrouet/catapulte/pull/204))
- *(deps)* Bump actix-web from 4.0.0-beta.11 to 4.0.0-beta.12 ([#199](https://github.com/jdrouet/catapulte/pull/199))
- *(deps)* Bump serde_json from 1.0.71 to 1.0.72 ([#203](https://github.com/jdrouet/catapulte/pull/203))
- *(deps)* Bump actix-rt from 2.4.0 to 2.5.0 ([#200](https://github.com/jdrouet/catapulte/pull/200))
- *(deps)* Bump futures from 0.3.17 to 0.3.18 ([#201](https://github.com/jdrouet/catapulte/pull/201))
- *(deps)* update dependencies
- *(deps)* Bump reqwest from 0.11.5 to 0.11.6 ([#190](https://github.com/jdrouet/catapulte/pull/190))
- *(deps)* Bump actix-rt from 2.2.0 to 2.3.0 ([#189](https://github.com/jdrouet/catapulte/pull/189))
- *(deps)* Bump mrml from 1.2.5 to 1.2.6 ([#188](https://github.com/jdrouet/catapulte/pull/188))
- *(deps)* Bump reqwest from 0.11.4 to 0.11.5 ([#187](https://github.com/jdrouet/catapulte/pull/187))
- *(ci)* only build canary images for amd64 ([#186](https://github.com/jdrouet/catapulte/pull/186))
- *(server)* update actix-web versions
- *(ci)* update timeout for building docker images
- *(ci)* build canary image ([#184](https://github.com/jdrouet/catapulte/pull/184))
- *(deps)* Bump mrml from 1.2.4 to 1.2.5
- *(deps)* Bump common-multipart-rfc7578 from 0.3.0 to 0.3.1
- *(deps)* Bump serde_json from 1.0.67 to 1.0.68
- *(smtp)* add SMTP_ACCEPT_INVALID_CERT variable
- *(version)* bump to version 0.4.2
- *(deps)* Bump handlebars from 4.1.2 to 4.1.3
- *(version)* bump to version 0.4.1
- *(deps)* Bump wiremock from 0.5.6 to 0.5.7
- *(deps)* Bump serde_json from 1.0.66 to 1.0.67
- *(deps)* Bump futures from 0.3.16 to 0.3.17
- *(deps)* Bump serde from 1.0.129 to 1.0.130
- *(deps)* Bump serde from 1.0.127 to 1.0.129
- *(deps)* Bump handlebars from 4.1.1 to 4.1.2
- *(deps)* Bump handlebars from 4.1.0 to 4.1.1
- *(deps)* Bump serde from 1.0.126 to 1.0.127
- *(deps)* Bump async-trait from 0.1.50 to 0.1.51
- *(deps)* Bump serde_json from 1.0.65 to 1.0.66
- *(deps)* Bump serde_json from 1.0.64 to 1.0.65
- *(deps)* Bump futures from 0.3.15 to 0.3.16
- *(deps)* Bump wiremock from 0.5.5 to 0.5.6
- *(deps)* Bump wiremock from 0.5.4 to 0.5.5
- *(deps)* Bump env_logger from 0.8.4 to 0.9.0
- *(deps)* Bump wiremock from 0.5.3 to 0.5.4
- *(deps)* Bump handlebars from 4.0.1 to 4.1.0
- *(deps)* Bump reqwest from 0.11.3 to 0.11.4
- *(deps)* Bump handlebars from 3.5.5 to 4.0.1
- *(deps)* Bump env_logger from 0.8.3 to 0.8.4
- *(deps)* Bump wiremock from 0.5.2 to 0.5.3
- *(deps)* Bump mrml from 1.2.3 to 1.2.4
- *(deps)* Bump mrml from 1.2.2 to 1.2.3
- *(deps)* Bump lettre from 0.10.0-rc.2 to 0.10.0-rc.3
- *(script)* update makefile to simplify running coverage localy
- *(server)* add AUTHENTICATION_ENABLED flag
- *(server)* update swagger
- *(wiki)* update environment variables documentation
- *(server)* split templates controller
- *(version)* bump to version 0.4.0
- *(script)* add multiarch dockerfile with alpine
- *(deps)* Bump lettre from 0.10.0-rc.1 to 0.10.0-rc.2
- *(server)* status endpoint now returns uptime
- *(multipart)* test when no filename is provided for attachment
- *(deps)* bump lettre to version 0.10.0-rc.1
- *(deps)* Bump mrml from 1.2.1 to 1.2.2
- *(script)* add liberapay username
- *(readme)* update funding links
- *(version)* bump to version 0.3.4
- *(ci)* add version to scope list
- *(deps)* Bump serde from 1.0.125 to 1.0.126
- *(deps)* Bump mrml from 1.2.0 to 1.2.1
- *(deps)* Bump handlebars from 3.5.4 to 3.5.5
- *(deps)* Bump futures from 0.3.14 to 0.3.15
- *(ci)* add commitizen build step
- adds example to readme.md
- adds SMTP_TLS_ENABLED env var info to the wiki.
- adds SMTP_TLS_ENABLED env var to readme.md
- add dependabot config
- *(deps)* Bump mrml from 1.1.0 to 1.2.0
- *(deps)* Bump async-trait from 0.1.49 to 0.1.50
- *(deps)* Bump mrml from 1.0.0 to 1.1.0
- *(deps)* Bump async-trait from 0.1.48 to 0.1.49
- *(deps)* Bump reqwest from 0.11.2 to 0.11.3
- *(deps)* Bump futures from 0.3.13 to 0.3.14
- *(deps)* Bump mrml 0.5.0 to 1.0.0
- *(deps)* Bump actix-rt 2.1.0 to 2.2.0
- *(deps)* Bump handlebars from 3.5.3 to 3.5.4
- *(deps)* Bump async-trait from 0.1.42 to 0.1.48
- *(deps)* Bump serde from 1.0.124 to 1.0.125
- *(deps)* Bump lettre from 0.10.0-beta.2 to 0.10.0-beta.3
- *(deps)* Bump wiremock from 0.5.1 to 0.5.2
- *(deps)* Bump reqwest from 0.11.1 to 0.11.2
- *(deps)* Bump lettre from 0.10.0-beta.1 to 0.10.0-beta.2
- *(deps)* Bump serde from 1.0.123 to 1.0.124
- *(deps)* Bump lettre from 0.10.0-alpha.5 to 0.10.0-beta.1
- *(deps)* Bump serde_json from 1.0.63 to 1.0.64
- *(deps)* Bump actix-rt from 2.0.2 to 2.1.0
- *(deps)* Bump serde_json from 1.0.62 to 1.0.63
- *(deps)* Bump wiremock from 0.5.0 to 0.5.1
- *(deps)* Bump wiremock from 0.4.9 to 0.5.0
- *(deps)* Bump futures from 0.3.12 to 0.3.13
- *(deps)* Bump handlebars from 3.5.2 to 3.5.3
- *(deps)* Bump reqwest from 0.11.0 to 0.11.1
- *(deps)* Bump common-multipart-rfc7578 from 0.2.0-rc to 0.3.0
- remove useless scripts and deps
- add integration testing
- *(version)* bump to version 0.3.3
- *(deps)* Bump env_logger from 0.8.2 to 0.8.3
- *(deps)* update lettre
- *(deps)* updating actix-* and reqwest
- use canary version in local docker-compose file
- *(deps)* Bump serde_json from 1.0.61 to 1.0.62
- *(deps)* Bump mrml from 0.4.0 to 0.5.0
- *(deps)* Bump wiremock from 0.4.8 to 0.4.9
- *(deps)* Bump log from 0.4.13 to 0.4.14
- *(deps)* Bump serde from 1.0.122 to 1.0.123
- *(deps)* Bump serde from 1.0.120 to 1.0.122
- *(deps)* Bump wiremock from 0.4.7 to 0.4.8
- *(deps)* Bump serde from 1.0.119 to 1.0.120
- *(deps)* Bump futures from 0.3.11 to 0.3.12
- *(deps)* Bump wiremock from 0.4.6 to 0.4.7
- *(deps)* Bump wiremock from 0.4.5 to 0.4.6
- *(deps)* Bump futures from 0.3.10 to 0.3.11
- *(deps)* Bump futures from 0.3.9 to 0.3.10
- *(deps)* Bump tempfile from 3.1.0 to 3.2.0
- *(deps)* Bump wiremock from 0.4.3 to 0.4.5
- *(deps)* Bump log from 0.4.11 to 0.4.13
- *(deps)* Bump uuid from 0.8.1 to 0.8.2
- *(deps)* Bump serde from 1.0.118 to 1.0.119
- *(deps)* Bump futures from 0.3.8 to 0.3.9
- *(deps)* Bump native-tls from 0.2.6 to 0.2.7
- *(deps)* Bump handlebars from 3.5.1 to 3.5.2
- *(deps)* Bump serde_json from 1.0.60 to 1.0.61
- *(deps)* Bump wiremock from 0.4.2 to 0.4.3
- *(deps)* Bump wiremock from 0.4.1 to 0.4.2
- *(deps)* Bump wiremock from 0.3.0 to 0.4.1
- *(deps)* Bump reqwest from 0.10.9 to 0.10.10
- add missing variable
- move to gitlab ci
- *(deps)* Bump serde from 1.0.117 to 1.0.118
- update serde_json
- avoid building dependabot's PR to keep credits
- *(version)* bump to 0.3.1
- update actix-web
- update dependencies
- update dependencies
- *(deps)* remove bytes dependency
- *(deps)* add Cargo.lock
- *(deps)* patch dependencies
- *(deps)* fix dependencies
- *(version)* 0.3.0
- *(clippy)* add clippy step on ci
- *(smtp)* explain how to use with amazon ses
- update branch for code coverage badge in readme
- *(server)* use env-test-util to manage envionment variables in tests
- *(asset)* add github social image source
- *(version)* 0.2.0
- add release script to makefile
- Merge pull request [#26](https://github.com/jdrouet/catapulte/pull/26) from jdrouet/travis-cleanup
- clean travis script
- *(deps)* update actix-*
- *(deps)* Update wiremock requirement from 0.2.4 to 0.3.0
- Merge pull request [#18](https://github.com/jdrouet/catapulte/pull/18) from jdrouet/dependabot/cargo/serial_test-0.5
- *(deps)* Update serial_test requirement from 0.4 to 0.5
- add funding button
- add badges to readme
- Merge pull request [#16](https://github.com/jdrouet/catapulte/pull/16) from jdrouet/deps-update
- upgrade mrml
- add license file
- replace circleci badge by travis badge
- move to travis
- Merge pull request [#12](https://github.com/jdrouet/catapulte/pull/12) from jdrouet/arm32v7
- create dockerfile working in multiarch for arm32v7
- create script to build with buildx
- upgrade mrml version
- disable image build, hanging...
- add default value for template description
- split image build to avoid being stuck
- update readme
- disable build that hangs up
- add docker-compose for local provider
- update readme
- build image on master
- add env variable for max pool
- change exclusion pattern
- implement provider
- fix variables
- compose variables instead of using url
- add how to use it section
- add script to build docker image
- update readme
- update mrml and add subject and text content
- put in place linter
- use same route for json and multipart
- allow to send email with attachment
- first commit

## [1.0.0] - 2024-03-05

### Build

- *(deps)* Bump clap from 4.4.12 to 4.4.13
- *(deps)* Bump clap from 4.4.13 to 4.4.14
- *(deps)* Bump serde from 1.0.194 to 1.0.195
- *(deps)* Bump utoipa from 4.1.0 to 4.2.0
- *(deps)* Bump utoipa-swagger-ui from 5.0.0 to 6.0.0
- *(deps)* Bump clap from 4.4.14 to 4.4.15
- *(deps)* Bump clap from 4.4.15 to 4.4.16
- *(deps)* Bump clap from 4.4.16 to 4.4.17
- *(deps)* Bump tower-http from 0.5.0 to 0.5.1
- *(deps)* Bump axum from 0.7.3 to 0.7.4
- *(deps)* Bump clap from 4.4.17 to 4.4.18
- *(deps)* Bump handlebars from 5.0.0 to 5.1.0
- *(deps)* Bump handlebars from 5.1.0 to 5.1.1
- *(deps)* Bump uuid from 1.6.1 to 1.7.0
- *(deps)* Bump h2 from 0.3.22 to 0.3.24
- *(deps)* Bump serde_json from 1.0.111 to 1.0.112
- *(deps)* Bump serde from 1.0.195 to 1.0.196
- *(deps)* Bump serde_json from 1.0.112 to 1.0.113
- *(deps)* Bump lettre from 0.11.3 to 0.11.4
- *(deps)* Bump reqwest from 0.11.23 to 0.11.24
- *(deps)* Bump config from 0.13.4 to 0.14.0
- *(deps)* Bump tokio from 1.35.1 to 1.36.0
- *(deps)* Bump tempfile from 3.9.0 to 3.10.0
- *(deps)* Bump clap from 4.4.18 to 4.5.0
- *(deps)* Bump wiremock from 0.5.22 to 0.6.0
- *(deps)* Bump metrics from 0.22.0 to 0.22.1
- *(deps)* Bump metrics-exporter-prometheus from 0.13.0 to 0.13.1
- *(deps)* Bump mrml from 3.0.0 to 3.0.1
- *(deps)* Bump clap from 4.5.0 to 4.5.1
- *(deps)* Bump serde_json from 1.0.113 to 1.0.114
- *(deps)* Bump serde from 1.0.196 to 1.0.197
- *(deps)* Bump tower-http from 0.5.1 to 0.5.2
- *(deps)* Bump tempfile from 3.10.0 to 3.10.1
- *(deps)* Bump mrml from 3.0.1 to 3.0.2
- *(deps)* Bump mrml from 3.0.2 to 3.0.3
- *(deps)* Bump mrml from 3.0.3 to 3.0.4
- *(deps)* Bump mio from 0.8.10 to 0.8.11

## [1.0.0-alpha.2] - 2024-01-04

### 🚀 Features

- Allow to disable color in logs
- *(serve)* Add graceful shutdown
- *(serve)* Add opportunity to have trace id logged for each request

### 🐛 Bug Fixes

- Dockerfile to reduce vulnerabilities
- Dockerfile to reduce vulnerabilities
- Remove double trace layer
- Dockerfile to reduce vulnerabilities

### 📚 Documentation

- Add example catapulte.toml file

### 🎨 Styling

- Apply lint with stable rust version

### ⚙️ Miscellaneous Tasks

- Please clippy
- Fix clippy suggestions
- Add some logs for creating smtp service
- *(ci)* Update events units
- Move from codebench to ci-metrics
- Change ci-metrics for alpine image
- Rename codebench config file
- Update funding
- Release
- Release

### Build

- *(deps)* Bump serde from 1.0.149 to 1.0.150
- *(deps)* Bump serde from 1.0.150 to 1.0.151
- *(deps)* Bump serde_json from 1.0.89 to 1.0.91
- *(deps)* Bump handlebars from 4.3.5 to 4.3.6
- *(deps)* Bump clap from 4.0.29 to 4.0.30
- *(deps)* Bump clap from 4.0.30 to 4.0.32
- *(deps)* Bump serde from 1.0.151 to 1.0.152
- *(deps)* Bump wiremock from 0.5.15 to 0.5.16
- *(deps)* Bump tokio from 1.23.0 to 1.23.1
- *(deps)* Bump tokio from 1.23.1 to 1.24.0
- *(deps)* Bump tokio from 1.24.0 to 1.24.1
- *(deps)* Bump axum from 0.6.1 to 0.6.2
- *(deps)* Bump mrml from 1.2.10 to 1.2.11
- *(deps)* Bump wiremock from 0.5.16 to 0.5.17
- *(deps)* Bump clap from 4.0.32 to 4.1.0
- *(deps)* Bump clap from 4.1.0 to 4.1.1
- *(deps)* Bump tokio from 1.24.1 to 1.24.2
- *(deps)* Bump reqwest from 0.11.13 to 0.11.14
- *(deps)* Bump axum from 0.6.2 to 0.6.3
- *(deps)* Bump clap from 4.1.1 to 4.1.3
- *(deps)* Bump axum from 0.6.3 to 0.6.4
- *(deps)* Bump clap from 4.1.3 to 4.1.4
- *(deps)* Bump tokio from 1.24.2 to 1.25.0
- *(deps)* Bum utoipa and utoipa to 3.0
- *(deps)* Bump uuid from 1.2.2 to 1.3.0
- *(deps)* Bump serde_json from 1.0.91 to 1.0.92
- *(deps)* Bump serde_json from 1.0.92 to 1.0.93
- *(deps)* Bump utoipa from 3.0.1 to 3.0.2
- *(deps)* Bump axum from 0.6.4 to 0.6.6
- *(deps)* Bump clap from 4.1.4 to 4.1.6
- *(deps)* Bump axum from 0.6.6 to 0.6.7
- *(deps)* Bump utoipa from 3.0.2 to 3.0.3
- *(deps)* Bump tower-http from 0.3.5 to 0.4.0
- *(deps)* Bump axum from 0.6.7 to 0.6.8
- *(deps)* Bump tempfile from 3.3.0 to 3.4.0
- *(deps)* Bump axum from 0.6.8 to 0.6.9
- *(deps)* Bump clap from 4.1.6 to 4.1.7
- *(deps)* Bump clap from 4.1.7 to 4.1.8
- *(deps)* Bump tokio from 1.25.0 to 1.26.0
- *(deps)* Bump axum from 0.6.9 to 0.6.10
- *(deps)* Bump serde_json from 1.0.93 to 1.0.94
- *(deps)* Bump serde from 1.0.152 to 1.0.153
- *(deps)* Bump serde from 1.0.153 to 1.0.154
- *(deps)* Bump utoipa from 3.0.3 to 3.1.0
- *(deps)* Bump utoipa-swagger-ui from 3.0.2 to 3.1.0
- *(deps)* Bump axum from 0.6.10 to 0.6.11
- *(deps)* Bump serde from 1.0.154 to 1.0.155
- *(deps)* Bump serde from 1.0.155 to 1.0.156
- *(deps)* Bump utoipa-swagger-ui from 3.1.0 to 3.1.1
- *(deps)* Bump utoipa from 3.1.0 to 3.1.1
- *(deps)* Bump clap from 4.1.8 to 4.1.9
- *(deps)* Bump utoipa from 3.1.1 to 3.1.2
- *(deps)* Bump clap from 4.1.9 to 4.1.11
- *(deps)* Bump reqwest from 0.11.14 to 0.11.15
- *(deps)* Bump serde from 1.0.156 to 1.0.158
- *(deps)* Bump axum from 0.6.11 to 0.6.12
- *(deps)* Bump serde_json from 1.0.94 to 1.0.95
- *(deps)* Bump tokio from 1.26.0 to 1.27.0
- *(deps)* Bump reqwest from 0.11.15 to 0.11.16
- *(deps)* Bump utoipa from 3.1.2 to 3.2.0
- *(deps)* Bump tempfile from 3.4.0 to 3.5.0
- *(deps)* Bump serde from 1.0.158 to 1.0.159
- *(deps)* Bump utoipa from 3.2.0 to 3.2.1
- *(deps)* Bump utoipa-swagger-ui from 3.1.1 to 3.1.2
- *(deps)* Bump wiremock from 0.5.17 to 0.5.18
- *(deps)* Bump utoipa-swagger-ui from 3.1.2 to 3.1.3
- *(deps)* Bump uuid from 1.3.0 to 1.3.1
- *(deps)* Bump serde from 1.0.159 to 1.0.160
- *(deps)* Bump axum from 0.6.12 to 0.6.14
- *(deps)* Bump serde_json from 1.0.95 to 1.0.96
- *(deps)* Bump axum from 0.6.14 to 0.6.15
- *(deps)* Bump h2 from 0.3.15 to 0.3.17
- *(deps)* Bump utoipa from 3.2.1 to 3.3.0
- *(deps)* Bump metrics-exporter-prometheus from 0.11.0 to 0.12.0
- *(deps)* Bump metrics from 0.20.1 to 0.21.0
- *(deps)* Bump axum from 0.6.15 to 0.6.16
- *(deps)* Bump tracing-subscriber from 0.3.16 to 0.3.17
- *(deps)* Bump tracing from 0.1.37 to 0.1.38
- *(deps)* Bump uuid from 1.3.1 to 1.3.2
- *(deps)* Bump axum from 0.6.16 to 0.6.18
- *(deps)* Bump reqwest from 0.11.16 to 0.11.17
- *(deps)* Bump tokio from 1.27.0 to 1.28.0
- *(deps)* Bump serde from 1.0.160 to 1.0.161
- *(deps)* Bump serde from 1.0.161 to 1.0.162
- *(deps)* Bump metrics-exporter-prometheus from 0.12.0 to 0.12.1
- *(deps)* Bump tokio from 1.28.0 to 1.28.1
- *(deps)* Bump handlebars from 4.3.6 to 4.3.7
- *(deps)* Bump serde from 1.0.162 to 1.0.163
- *(deps)* Bump uuid from 1.3.2 to 1.3.3
- *(deps)* Bump reqwest from 0.11.17 to 0.11.18
- *(deps)* Bump tokio from 1.28.1 to 1.28.2
- *(deps)* Bump tempfile from 3.5.0 to 3.6.0
- *(deps)* Bump wiremock from 0.5.18 to 0.5.19
- *(deps)* Bump serde from 1.0.163 to 1.0.164
- *(deps)* Bump uuid from 1.3.3 to 1.3.4
- *(deps)* Bump serde_json from 1.0.96 to 1.0.97
- *(deps)* Bump tower-http from 0.4.0 to 0.4.1
- *(deps)* Bump serde_json from 1.0.97 to 1.0.99
- *(deps)* Bump uuid from 1.3.4 to 1.4.0
- *(deps)* Bump tokio from 1.28.2 to 1.29.0
- *(deps)* Bump tokio from 1.29.0 to 1.29.1
- *(deps)* Bump serde_json from 1.0.99 to 1.0.100
- *(deps)* Bump metrics from 0.21.0 to 0.21.1
- *(deps)* Bump clap from 4.1.11 to 4.3.10
- *(deps)* Bump clap from 4.3.10 to 4.3.11
- *(deps)* Bump serde from 1.0.166 to 1.0.167
- *(deps)* Bump serde from 1.0.167 to 1.0.171
- *(deps)* Bump serde_json from 1.0.100 to 1.0.101
- *(deps)* Bump serde_json from 1.0.101 to 1.0.102
- *(deps)* Bump utoipa from 3.3.0 to 3.4.0
- *(deps)* Bump utoipa-swagger-ui from 3.1.3 to 3.1.4
- *(deps)* Bump clap from 4.3.11 to 4.3.12
- *(deps)* Bump hyper from 0.14.24 to 0.14.26
- *(deps)* Bump axum from 0.6.18 to 0.6.19
- *(deps)* Bump uuid from 1.4.0 to 1.4.1
- *(deps)* Bump clap from 4.3.12 to 4.3.16
- *(deps)* Bump hyper from 0.14.26 to 0.14.27
- *(deps)* Bump serde_json from 1.0.102 to 1.0.103
- *(deps)* Bump tower-http from 0.4.1 to 0.4.2
- *(deps)* Bump tempfile from 3.6.0 to 3.7.0
- *(deps)* Bump tower-http from 0.4.2 to 0.4.3
- *(deps)* Bump clap from 4.3.16 to 4.3.17
- *(deps)* Bump serde from 1.0.171 to 1.0.174
- *(deps)* Bump clap from 4.3.17 to 4.3.19
- *(deps)* Bump utoipa from 3.4.0 to 3.4.2
- *(deps)* Bump utoipa from 3.4.2 to 3.4.3
- *(deps)* Bump serde from 1.0.174 to 1.0.175
- *(deps)* Bump serde from 1.0.175 to 1.0.176
- *(deps)* Bump serde_json from 1.0.103 to 1.0.104
- *(deps)* Bump serde from 1.0.176 to 1.0.177
- *(deps)* Bump serde from 1.0.177 to 1.0.178
- *(deps)* Bump serde from 1.0.178 to 1.0.180
- *(deps)* Bump axum from 0.6.19 to 0.6.20
- *(deps)* Bump serde from 1.0.180 to 1.0.181
- *(deps)* Bump utoipa from 3.4.3 to 3.4.4
- *(deps)* Bump serde from 1.0.181 to 1.0.183
- *(deps)* Bump tempfile from 3.7.0 to 3.7.1
- *(deps)* Bump utoipa-swagger-ui from 3.1.4 to 3.1.5
- *(deps)* Bump clap from 4.3.19 to 4.3.21
- *(deps)* Bump tokio from 1.29.1 to 1.30.0
- *(deps)* Bump tokio from 1.30.0 to 1.31.0
- *(deps)* Bump serde_json from 1.0.104 to 1.0.105
- *(deps)* Run cargo update to bump deps
- *(docker)* Bump debian tag from bullseye to bookworm
- *(deps)* Bump tokio from 1.31.0 to 1.32.0
- *(deps)* Bump lettre to 0.10
- *(deps)* Bump clap from 4.3.22 to 4.3.23
- *(deps)* Bump tempfile from 3.7.1 to 3.8.0
- *(deps)* Bump serde from 1.0.183 to 1.0.185
- *(deps)* Bump reqwest from 0.11.18 to 0.11.19
- *(deps)* Bump utoipa from 3.4.4 to 3.5.0
- *(deps)* Bump rustls-webpki from 0.100.1 to 0.100.2
- *(deps)* Bump clap from 4.3.23 to 4.3.24
- *(deps)* Bump reqwest from 0.11.19 to 0.11.20
- *(deps)* Bump clap from 4.3.24 to 4.4.0
- *(deps)* Bump serde from 1.0.185 to 1.0.186
- *(deps)* Bump serde from 1.0.186 to 1.0.187
- *(deps)* Bump clap from 4.4.0 to 4.4.1
- *(deps)* Bump serde from 1.0.187 to 1.0.188
- *(deps)* Bump clap from 4.4.1 to 4.4.2
- *(deps)* Bump tower-http from 0.4.3 to 0.4.4
- *(deps)* Bump handlebars from 4.3.7 to 4.4.0
- *(deps)* Bump serde_json from 1.0.105 to 1.0.106
- *(deps)* Bump clap from 4.4.2 to 4.4.3
- *(deps)* Bump serde_json from 1.0.106 to 1.0.107
- *(deps)* Bump clap from 4.4.3 to 4.4.4
- *(deps)* Bump clap from 4.4.4 to 4.4.5
- *(deps)* Bump clap from 4.4.5 to 4.4.6
- *(ci)* Publish docker image size
- *(deps)* Bump reqwest from 0.11.20 to 0.11.21
- *(deps)* Bump reqwest from 0.11.21 to 0.11.22
- *(deps)* Update dependencies
- *(ci)* Automagically create pr
- *(ci)* Add missing fetch-depth
- *(deps)* Bump tokio from 1.32.0 to 1.33.0
- *(deps)* Bump utoipa and utoipa-swagger-ui to 4.0
- *(deps)* Bump tracing from 0.1.37 to 0.1.39
- *(deps)* Bump serde from 1.0.188 to 1.0.189
- *(deps)* Bump uuid from 1.4.1 to 1.5.0
- *(deps)* Bump rustix from 0.38.17 to 0.38.19
- *(deps)* Bump tracing from 0.1.39 to 0.1.40
- *(deps)* Bump clap from 4.4.6 to 4.4.7
- *(deps)* Bump tempfile from 3.8.0 to 3.8.1
- *(deps)* Bump serde from 1.0.189 to 1.0.190
- *(deps)* Bump serde_json from 1.0.107 to 1.0.108
- *(ci)* Update command to push metrics
- *(deps)* Bump wiremock from 0.5.19 to 0.5.21
- *(deps)* Bump serde from 1.0.190 to 1.0.191
- *(deps)* Bump serde from 1.0.191 to 1.0.192
- *(deps)* Bump clap from 4.4.7 to 4.4.8
- *(deps)* Bump tokio from 1.33.0 to 1.34.0
- *(deps)* Bump handlebars from 4.4.0 to 4.5.0
- *(deps)* Bump tracing-subscriber from 0.3.17 to 0.3.18
- *(deps)* Bump utoipa from 4.0.0 to 4.1.0
- *(deps)* Bump uuid from 1.5.0 to 1.6.1
- *(deps)* Bump serde from 1.0.192 to 1.0.193
- *(deps)* Bump config from 0.13.3 to 0.13.4
- *(deps)* Bump clap from 4.4.8 to 4.4.9
- *(deps)* Bump hyper from 0.14.27 to 1.0.1
- *(deps)* Fully remove hyper
- *(deps)* Bump clap from 4.4.9 to 4.4.10
- *(deps)* Bump wiremock from 0.5.21 to 0.5.22
- *(deps)* Bump lettre to 0.11
- *(deps)* Bump mrml to 2.0
- Update release-plz config
- Update release-plz config
- *(deps)* Bump deps
- *(deps)* Bump tokio from 1.34.0 to 1.35.0
- *(deps)* Bump zerocopy from 0.7.28 to 0.7.31
- *(deps)* Bump axum and related
- *(deps)* Bump metrics and related
- *(deps)* Bump handlerbars
- *(deps)* Bump mrml

## [1.0.0-alpha-1] - 2022-12-11

### 🚀 Features

- *(server)* Add cli options (#181)
- *(axum)* Simplify application with axum
- *(axum)* Generate openapi
- *(axum)* Add some logs and metrics
- *(axum)* Update dockerfile to add healthcheck
- *(axum)* Update openapi definitions
- *(axum)* Update errors and tests
- *(axum)* Delete swagger folder
- *(axum)* Update documentation
- *(axum)* Remove unused jolimail provider
- *(axum)* Implement more tests for json handler
- *(axum)* Add more tests
- *(axum)* Remove useless deps
- *(openapi)* Create a command to print the openapi json schema
- *(provider)* Update local provider
- *(provider)* Create a new http provider
- *(provider)* Add some tests

### 🐛 Bug Fixes

- *(ci)* Avoid hanging on apk add (#236)
- *(ci)* Upgrade buildx and docker dind
- *(ci)* Replace repository url to avoid hang up
- *(ci)* Replace repository url to avoid hang up
- *(ci)* Use network mode to host when building images
- Multiarch-alpine.Dockerfile to reduce vulnerabilities (#165)
- Alpine.Dockerfile to reduce vulnerabilities (#297)
- *(ci)* Use list of string for tags
- *(ci)* Use good name for dockerfile
- *(ci)* Rename dockerfiles
- *(ci)* Specify buildx platforms
- *(ci)* Only build for amd64
- *(ci)* Install buildx to use docker-compose
- *(ci)* Force using buildkit
- *(ci)* Update dockerfile to make tests run
- *(ci)* Use concurrency to cancel running jobs
- *(test)* Update TEMPLATE_PATH in compose file
- *(build)* Remove swagger from dockerfiles
- Remove unused enum item

### 📚 Documentation

- *(smtp)* Add SMTP_ACCEPT_INVALID_CERT variable

### ⚙️ Miscellaneous Tasks

- *(server)* Update actix-web versions
- *(ci)* Only build canary images for amd64 (#186)
- *(ci)* Migrate to github actions
- *(ci)* Prepare release workflow
- *(ci)* Limit concurrency for main worflow
- *(ci)* Make release build triggerable manually
- *(ci)* Add stage to build binaries
- *(ci)* Specify version to trigger release manually
- *(ci)* Update platforms
- *(ci)* Install qemu
- *(ci)* Take binaries out of docker context
- *(ci)* Make testing job go faster
- *(ci)* Use workflow file to trigger build
- *(ci)* Release canary and version when tagged

### Build

- *(deps)* Bump serde_json from 1.0.67 to 1.0.68
- *(deps)* Bump common-multipart-rfc7578 from 0.3.0 to 0.3.1
- *(deps)* Bump mrml from 1.2.4 to 1.2.5
- *(ci)* Build canary image (#184)
- *(ci)* Update timeout for building docker images
- *(deps)* Bump reqwest from 0.11.4 to 0.11.5 (#187)
- *(deps)* Bump mrml from 1.2.5 to 1.2.6 (#188)
- *(deps)* Bump actix-rt from 2.2.0 to 2.3.0 (#189)
- *(deps)* Bump reqwest from 0.11.5 to 0.11.6 (#190)
- *(deps)* Update dependencies
- *(deps)* Bump futures from 0.3.17 to 0.3.18 (#201)
- *(deps)* Bump actix-rt from 2.4.0 to 2.5.0 (#200)
- *(deps)* Bump serde_json from 1.0.71 to 1.0.72 (#203)
- *(deps)* Bump actix-web from 4.0.0-beta.11 to 4.0.0-beta.12 (#199)
- *(deps)* Bump funty from 1.1.0 to 2.0.0 (#204)
- *(deps)* Bump actix-web from 4.0.0-beta.12 to 4.0.0-beta.13 (#205)
- *(deps)* Bump reqwest from 0.11.6 to 0.11.7 (#206)
- *(deps)* Bump handlebars from 4.1.5 to 4.1.6
- *(deps)* Bump async-trait from 0.1.51 to 0.1.52 (#211)
- *(deps)* Bump serde_json from 1.0.72 to 1.0.73 (#217)
- *(deps)* Bump serde from 1.0.130 to 1.0.131 (#212)
- *(deps)* Bump actix packages (#220)
- *(deps)* Bump lettre to 0.10.0-rc4 (#223)
- *(deps)* Bump reqwest from 0.11.7 to 0.11.8 (#226)
- *(deps)* Bump futures from 0.3.17 to 0.3.19 (#227)
- *(deps)* Bump actix-rt from 2.5.0 to 2.5.1 (#234)
- *(deps)* Bump clap from 3.0.0-beta.5 to 3.0.0 (#235)
- *(deps)* Bump clap from 3.0.0 to 3.0.1 (#239)
- *(deps)* Bump serde from 1.0.132 to 1.0.133 (#237)
- *(deps)* Bump clap from 3.0.1 to 3.0.4 (#240)
- *(deps)* Bump serde_json from 1.0.73 to 1.0.74 (#238)
- *(deps)* Bump wiremock from 0.5.8 to 0.5.9 (#243)
- *(deps)* Bump handlebars from 4.1.6 to 4.2.0 (#244)
- *(deps)* Bump clap from 3.0.4 to 3.0.5 (#245)
- *(deps)* Bump actix-web, actix-http and actix-multipart (#246)
- *(deps)* Bump wiremock from 0.5.9 to 0.5.10 (#247)
- *(deps)* Bump clap from 3.0.5 to 3.0.6 (#249)
- *(deps)* Bump reqwest from 0.11.8 to 0.11.9 (#248)
- *(deps)* Bump clap from 3.0.6 to 3.0.7 (#250)
- *(deps)* Bump actix-rt from 2.5.1 to 2.6.0 (#251)
- *(deps)* Bump actix-web from 4.0.0-beta.19 to 4.0.0-beta.20
- *(deps)* Bump clap from 3.0.7 to 3.0.8 (#253)
- *(deps)* Bump handlebars from 4.2.0 to 4.2.1 (#255)
- *(deps)* Bump serde_json from 1.0.74 to 1.0.75 (#254)
- *(deps)* Bump clap from 3.0.8 to 3.0.10 (#256)
- *(deps)* Bump serde from 1.0.133 to 1.0.134
- *(deps)* Bump actix-http from 3.0.0-beta.18 to 3.0.0-beta.19
- *(deps)* Bump actix-web from 4.0.0-beta.20 to 4.0.0-beta.21 (#262)
- *(deps)* Bump clap from 3.0.10 to 3.0.12 (#261)
- *(deps)* Bump serde from 1.0.134 to 1.0.135 (#260)
- *(deps)* Bump serde_json from 1.0.75 to 1.0.78 (#259)
- *(deps)* Bump serde from 1.0.135 to 1.0.136 (#263)
- *(deps)* Bump clap from 3.0.12 to 3.0.13 (#264)
- *(deps)* Bump mrml from 1.2.6 to 1.2.7 (#265)
- *(deps)* Bump clap from 3.0.13 to 3.0.14 (#267)
- *(deps)* Bump actix-http from 3.0.0-beta.19 to 3.0.0-rc.1 (#266)
- *(deps)* Bump jsonwebtoken from 7.2.0 to 8.0.0 (#268)
- *(deps)* Bump jsonwebtoken from 8.0.0 to 8.0.1
- *(deps)* Bump futures from 0.3.19 to 0.3.21 (#270)
- *(deps)* Bump actix-http from 3.0.0-rc.1 to 3.0.0-rc.2
- *(deps)* Bump actix-web from 4.0.0-rc.2 to 4.0.0-rc.3 (#273)
- *(deps)* Bump serde_json from 1.0.78 to 1.0.79 (#274)
- *(deps)* Bump clap from 3.0.14 to 3.1.0 (#275)
- *(deps)* Bump actix-http from 3.0.0-rc.2 to 3.0.0-rc.3 (#276)
- *(deps)* Bump serial_test from 0.5.1 to 0.6.0 (#278)
- *(deps)* Bump clap from 3.1.0 to 3.1.2 (#280)
- *(deps)* Bump actix-http from 3.0.0-rc.3 to 3.0.0-rc.4 (#279)
- *(deps)* Bump actix-http from 3.0.0-rc.4 to 3.0.0 (#281)
- *(deps)* Bump clap from 3.1.2 to 3.1.3 (#283)
- *(deps)* Bump wiremock from 0.5.10 to 0.5.11 (#282)
- *(deps)* Bump clap from 3.1.3 to 3.1.5 (#284)
- *(deps)* Bump actix-http from 3.0.0 to 3.0.1 (#286)
- *(deps)* Bump actix-http from 3.0.1 to 3.0.2 (#288)
- *(deps)* Bump clap from 3.1.5 to 3.1.6 (#287)
- *(deps)* Bump actix-http from 3.0.2 to 3.0.3 (#289)
- *(deps)* Bump actix-rt from 2.6.0 to 2.7.0 (#290)
- *(deps)* Bump actix-http from 3.0.3 to 3.0.4 (#291)
- *(deps)* Bump handlebars from 4.2.1 to 4.2.2 (#292)
- *(deps)* Bump mrml from 1.2.7 to 1.2.8 (#294)
- *(deps)* Bump reqwest from 0.11.9 to 0.11.10 (#295)
- *(deps)* Bump log from 0.4.14 to 0.4.16 (#299)
- *(deps)* Bump mrml from 1.2.8 to 1.2.9 (#298)
- *(deps)* Bump async-trait from 0.1.52 to 0.1.53
- *(deps)* Bump mrml from 1.2.9 to 1.2.10
- *(deps)* Bump jsonwebtoken from 8.0.1 to 8.1.0 (#306)
- *(deps)* Bump lettre from 0.10.0-rc.4 to 0.10.0-rc.5 (#304)
- *(deps)* Bump clap from 3.1.6 to 3.1.9 (#307)
- *(deps)* Bump wiremock from 0.5.11 to 0.5.12 (#305)
- *(deps)* Bump common-multipart-rfc7578 from 0.3.1 to 0.5.0 (#296)
- *(deps)* Bump clap from 3.1.9 to 3.1.10 (#309)
- *(deps)* Bump wiremock from 0.5.12 to 0.5.13 (#308)
- *(deps)* Bump uuid from 0.8.2 to 1.0.0 (#310)
- *(deps)* Bump clap from 3.1.10 to 3.1.12 (#311)
- *(deps)* Bump lettre from 0.10.0-rc.5 to 0.10.0-rc.6 (#312)
- *(deps)* Bump clap to 4.0.10 (#340)
- *(deps)* Bump serial_test to 0.9 (#341)
- *(deps)* Bump common-multipart-rfc7578 from 0.5.0 to 0.6.0
- *(deps)* Bump alpine image version (#342)
- *(docker)* Bump docker images to bulleye (#343)
- *(deps)* Upgrade actix deps (#344)
- *(deps)* Bump serde_json from 1.0.85 to 1.0.86 (#350)
- *(deps)* Bump uuid from 1.1.2 to 1.2.1 (#349)
- *(deps)* Bump wiremock from 0.5.14 to 0.5.15 (#347)
- *(deps)* Bump clap from 4.0.10 to 4.0.14
- *(deps)* Bump clap from 4.0.14 to 4.0.15
- *(deps)* Bump clap from 4.0.15 to 4.0.17
- *(deps)* Bump async-trait from 0.1.57 to 0.1.58
- *(deps)* Bump serde_json from 1.0.86 to 1.0.87
- *(deps)* Bump futures from 0.3.24 to 0.3.25
- *(deps)* Bump clap from 4.0.17 to 4.0.18
- *(deps)* Bump serde from 1.0.145 to 1.0.147
- *(deps)* Bump clap from 4.0.18 to 4.0.22
- *(deps)* Bump env_logger from 0.9.1 to 0.9.3
- *(deps)* Bump clap from 4.0.22 to 4.0.23
- *(deps)* Bump uuid from 1.2.1 to 1.2.2
- *(deps)* Bump clap from 4.0.23 to 4.0.24
- *(deps)* Bump clap from 4.0.24 to 4.0.26
- *(deps)* Bump reqwest from 0.11.12 to 0.11.13
- *(deps)* Bump serde_json from 1.0.87 to 1.0.89
- *(deps)* Bump clap from 4.0.26 to 4.0.27
- *(deps)* Bump tower-http from 0.3.4 to 0.3.5
- *(deps)* Bump serde from 1.0.148 to 1.0.149
- *(deps)* Bump config from 0.13.2 to 0.13.3
- *(deps)* Bump tokio from 1.22.0 to 1.23.0

## [0.4.2] - 2021-09-14

### 🚀 Features

- *(smtp)* Allow to connect with invalid cert
- *(smtp)* Ensure invalid tls is working
- *(server)* Run tests in docker-compose
- *(smtp)* Fix tests

### ⚙️ Miscellaneous Tasks

- *(version)* Bump to version 0.4.2

### Build

- *(deps)* Bump handlebars from 4.1.2 to 4.1.3

## [0.4.1] - 2021-09-12

### 🚀 Features

- *(server)* Add authentication service
- *(server)* Create authentication middleware

### 🐛 Bug Fixes

- *(server)* Add Bearer in the authentication header
- *(server)* Make status endpoint visible
- *(ci)* Allow coverage to fail
- *(ci)* Apply clippy's proposals
- *(server)* Stop returning 404 where swagger enabled

### 🚜 Refactor

- *(server)* Split templates controller
- *(server)* Add AUTHENTICATION_ENABLED flag
- *(script)* Update makefile to simplify running coverage localy

### 📚 Documentation

- *(wiki)* Update environment variables documentation
- *(server)* Update swagger

### ⚙️ Miscellaneous Tasks

- *(version)* Bump to version 0.4.1

### Build

- *(deps)* Bump lettre from 0.10.0-rc.2 to 0.10.0-rc.3
- *(deps)* Bump mrml from 1.2.2 to 1.2.3
- *(deps)* Bump mrml from 1.2.3 to 1.2.4
- *(deps)* Bump wiremock from 0.5.2 to 0.5.3
- *(deps)* Bump env_logger from 0.8.3 to 0.8.4
- *(deps)* Bump handlebars from 3.5.5 to 4.0.1
- *(deps)* Bump reqwest from 0.11.3 to 0.11.4
- *(deps)* Bump handlebars from 4.0.1 to 4.1.0
- *(deps)* Bump wiremock from 0.5.3 to 0.5.4
- *(deps)* Bump env_logger from 0.8.4 to 0.9.0
- *(deps)* Bump wiremock from 0.5.4 to 0.5.5
- *(deps)* Bump wiremock from 0.5.5 to 0.5.6
- *(deps)* Bump futures from 0.3.15 to 0.3.16
- *(deps)* Bump serde_json from 1.0.64 to 1.0.65
- *(deps)* Bump serde_json from 1.0.65 to 1.0.66
- *(deps)* Bump async-trait from 0.1.50 to 0.1.51
- *(deps)* Bump serde from 1.0.126 to 1.0.127
- *(deps)* Bump handlebars from 4.1.0 to 4.1.1
- *(deps)* Bump handlebars from 4.1.1 to 4.1.2
- *(deps)* Bump serde from 1.0.127 to 1.0.129
- *(deps)* Bump serde from 1.0.129 to 1.0.130
- *(deps)* Bump futures from 0.3.16 to 0.3.17
- *(deps)* Bump serde_json from 1.0.66 to 1.0.67
- *(deps)* Bump wiremock from 0.5.6 to 0.5.7

## [0.4.0] - 2021-05-19

### 🚀 Features

- *(server)* Add openapi into swagger folder
- *(server)* Add controller for swagger
- *(server)* Hide swagger behing environment variable
- *(server)* Update openapi to add cc, bcc and attachments

### 📚 Documentation

- *(readme)* Update funding links

### ⚙️ Miscellaneous Tasks

- *(script)* Add liberapay username
- *(multipart)* Test when no filename is provided for attachment
- *(script)* Add multiarch dockerfile with alpine
- *(version)* Bump to version 0.4.0

### Build

- *(deps)* Bump mrml from 1.2.1 to 1.2.2
- *(deps)* Bump lettre from 0.10.0-rc.1 to 0.10.0-rc.2

### Enhancement

- *(server)* Status endpoint now returns uptime

## [0.3.4] - 2021-05-13

### 🚀 Features

- *(rustls)* Replace native-tls by rustls
- Add alpine dockerfile

### 🐛 Bug Fixes

- Please clippy
- *(lint)* Please clippy
- *(docker)* Remove Cargo.lock from dockerignore
- Reorder struct attributes
- *(server)* Fix arm64 build with docker

### 📚 Documentation

- Adds SMTP_TLS_ENABLED env var to readme.md
- Adds SMTP_TLS_ENABLED env var info to the wiki.
- Adds example to readme.md

### ⚙️ Miscellaneous Tasks

- Add integration testing
- Remove useless scripts and deps
- Add dependabot config
- *(ci)* Add commitizen build step
- *(ci)* Add version to scope list
- *(version)* Bump to version 0.3.4

### Build

- *(deps)* Bump common-multipart-rfc7578 from 0.2.0-rc to 0.3.0
- *(deps)* Bump reqwest from 0.11.0 to 0.11.1
- *(deps)* Bump handlebars from 3.5.2 to 3.5.3
- *(deps)* Bump futures from 0.3.12 to 0.3.13
- *(deps)* Bump wiremock from 0.4.9 to 0.5.0
- *(deps)* Bump wiremock from 0.5.0 to 0.5.1
- *(deps)* Bump serde_json from 1.0.62 to 1.0.63
- *(deps)* Bump actix-rt from 2.0.2 to 2.1.0
- *(deps)* Bump serde_json from 1.0.63 to 1.0.64
- *(deps)* Bump lettre from 0.10.0-alpha.5 to 0.10.0-beta.1
- *(deps)* Bump serde from 1.0.123 to 1.0.124
- *(deps)* Bump lettre from 0.10.0-beta.1 to 0.10.0-beta.2
- *(deps)* Bump reqwest from 0.11.1 to 0.11.2
- *(deps)* Bump wiremock from 0.5.1 to 0.5.2
- *(deps)* Bump lettre from 0.10.0-beta.2 to 0.10.0-beta.3
- *(deps)* Bump serde from 1.0.124 to 1.0.125
- *(deps)* Bump async-trait from 0.1.42 to 0.1.48
- *(deps)* Bump handlebars from 3.5.3 to 3.5.4
- *(deps)* Bump actix-rt 2.1.0 to 2.2.0
- *(deps)* Bump mrml 0.5.0 to 1.0.0
- *(deps)* Bump futures from 0.3.13 to 0.3.14
- *(deps)* Bump reqwest from 0.11.2 to 0.11.3
- *(deps)* Bump async-trait from 0.1.48 to 0.1.49
- *(deps)* Bump mrml from 1.0.0 to 1.1.0
- *(deps)* Bump async-trait from 0.1.49 to 0.1.50
- *(deps)* Bump mrml from 1.1.0 to 1.2.0
- *(deps)* Bump futures from 0.3.14 to 0.3.15
- *(deps)* Bump handlebars from 3.5.4 to 3.5.5
- *(deps)* Bump mrml from 1.2.0 to 1.2.1
- *(deps)* Bump serde from 1.0.125 to 1.0.126

## [0.3.3] - 2021-02-13

### 🐛 Bug Fixes

- *(test)* Replace localhost by 127.0.0.1 for local tests
- Apply clippy suggestions
- *(build)* Pin version to allow build in docker

### ⚙️ Miscellaneous Tasks

- Avoid building dependabot's PR to keep credits
- Update serde_json
- Move to gitlab ci
- Add missing variable
- Use canary version in local docker-compose file
- *(version)* Bump to version 0.3.3

### Build

- *(deps)* Bump serde from 1.0.117 to 1.0.118
- *(deps)* Bump reqwest from 0.10.9 to 0.10.10
- *(deps)* Bump wiremock from 0.3.0 to 0.4.1
- *(deps)* Bump wiremock from 0.4.1 to 0.4.2
- *(deps)* Bump wiremock from 0.4.2 to 0.4.3
- *(deps)* Bump serde_json from 1.0.60 to 1.0.61
- *(deps)* Bump handlebars from 3.5.1 to 3.5.2
- *(deps)* Bump native-tls from 0.2.6 to 0.2.7
- *(deps)* Bump futures from 0.3.8 to 0.3.9
- *(deps)* Bump serde from 1.0.118 to 1.0.119
- *(deps)* Bump uuid from 0.8.1 to 0.8.2
- *(deps)* Bump log from 0.4.11 to 0.4.13
- *(deps)* Bump wiremock from 0.4.3 to 0.4.5
- *(deps)* Bump tempfile from 3.1.0 to 3.2.0
- *(deps)* Bump futures from 0.3.9 to 0.3.10
- *(deps)* Bump futures from 0.3.10 to 0.3.11
- *(deps)* Bump wiremock from 0.4.5 to 0.4.6
- *(deps)* Bump wiremock from 0.4.6 to 0.4.7
- *(deps)* Bump futures from 0.3.11 to 0.3.12
- *(deps)* Bump serde from 1.0.119 to 1.0.120
- *(deps)* Bump wiremock from 0.4.7 to 0.4.8
- *(deps)* Bump serde from 1.0.120 to 1.0.122
- *(deps)* Bump serde from 1.0.122 to 1.0.123
- *(deps)* Bump log from 0.4.13 to 0.4.14
- *(deps)* Bump wiremock from 0.4.8 to 0.4.9
- *(deps)* Bump mrml from 0.4.0 to 0.5.0
- *(deps)* Bump serde_json from 1.0.61 to 1.0.62
- *(deps)* Bump env_logger from 0.8.2 to 0.8.3

## [0.3.1] - 2020-12-02

### 🚀 Features

- Can set multiple emails in to, cc and bcc

### 🐛 Bug Fixes

- *(multipart)* Use alternative method

### 🚜 Refactor

- *(deps)* Remove bytes dependency

### ⚙️ Miscellaneous Tasks

- Update dependencies
- Update dependencies
- Update actix-web
- *(version)* Bump to 0.3.1

## [0.3.0] - 2020-11-14

### 🚀 Features

- *(deps)* Upgrade mrml to version 0.4.0
- *(template)* Load MRML options from environment variables
- *(heroku)* Use container stack and define required files
- *(smtp)* Handle tls connection

### 🐛 Bug Fixes

- Use current version as X-Version header
- *(test)* Remove cargo cache hanging
- *(variable)* Update docs around environment variables

### 🚜 Refactor

- *(server)* Use env-test-util to manage envionment variables in tests

### 📚 Documentation

- Add button to deploy to heroku
- Replace circleci badge by travis badge
- Add license file
- Add badges to readme
- Add funding button
- Create doc around environment variables
- Create openapi file
- *(asset)* Add github social image source
- Update branch for code coverage badge in readme
- *(smtp)* Explain how to use with amazon ses

### 🧪 Testing

- *(clippy)* Add clippy step on ci

### ⚙️ Miscellaneous Tasks

- Move to travis
- Clean travis script
- *(version)* 0.2.0
- *(version)* 0.3.0

### Build

- Create dockerfile working in multiarch for arm32v7
- *(deps)* Update env_logger requirement from 0.7 to 0.8
- *(deps)* Update serial_test requirement from 0.4 to 0.5
- *(deps)* Update wiremock requirement from 0.2.4 to 0.3.0
- *(deps)* Update actix-*
- Add release script to makefile

### Clean

- Apply clippy changes

### Deps

- Upgrade mrml

## [0.1.0] - 2020-10-01

### 🚀 Features

- *(smtp)* Create connection to smtp
- *(templates)* Can send local template

### 🐛 Bug Fixes

- Prefix requests to jolimail with /api

### 📚 Documentation

- Update readme
- Add how to use it section
- Update readme
- Add docker-compose for local provider
- Update readme

### ⚙️ Miscellaneous Tasks

- Put in place linter
- Fix variables
- Build image on master
- Disable build that hangs up
- Split image build to avoid being stuck
- Disable image build, hanging...

### Attachment

- Allow to send email with attachment
- Use same route for json and multipart

### Build

- Add script to build docker image
- Create script to build with buildx

### Coverage

- Change exclusion pattern

### Deps

- Update mrml and add subject and text content
- Upgrade mrml version

### Init

- First commit

### Jolimail

- Implement provider

### Server

- Add default value for template description

### Smtp

- Compose variables instead of using url
- Add env variable for max pool

<!-- generated by git-cliff -->
