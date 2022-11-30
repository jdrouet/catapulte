# Environment variables and arguments

Catapulte can be configured using CLI arguments or environment variables.
Those arguments/variables can be seen by running catapulte with `--help`.

## Server relative

- `LOG` is the level of log used to trace the application, default to `INFO`.
- `HOST` is where the server will listen to, default to `0.0.0.0` in container otherwise `localhost`
- `PORT` is the port on white the server will listen to, default to `3000`

## Template relative

- `TEMPLATE__TYPE` defines the type of provider used by this instance. `local` is the only option for now.

When the provider `local` is used

- `TEMPLATE__PATH` is the path where the templates will be loaded. In the container, the default is `/templates` otherwise it's `./templates`.

## MRML relative

- `RENDER__KEEP_COMMENTS` is a flag defining if MRML should keep the comments.
- `RENDER__SOCIAL_ICON_ORIGIN` is the base URL to load the social icons for `mj-social-element`. It's the default MRML value (`https://www.mailjet.com/images/theme/v1/icons/ico-social/`).

## SMTP relative

- `SMTP__HOSTNAME` is the hostname of the SMTP server (default `localhost`)
- `SMTP__PORT` is the port of the SMTP server (default `25`)
- `SMTP__USERNAME` is the username used to authenticate with the SMTP server
- `SMTP__PASSWORD` is the password used to authenticate with the SMTP server
- `SMTP__MAX_POOL_SIZE` is the max number of connection to the SMTP server (default `10`)
- `SMTP__TLS_ENABLED` enables TLS secure connection to the SMTP server (default `false`)
- `SMTP__ACCEPT_INVALID_CERT` allow the smtp client to accept invalid certificates (default `false`)
