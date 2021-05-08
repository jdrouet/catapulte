# Environment variables

Catapulte use several environment variables to be configured.

## Server relative

- `ADDRESS` is where the server will listen to, default to `0.0.0.0` in container otherwise `localhost`
- `PORT` is the port on white the server will listen to, default to `3000`

## Template relative

- `TEMPLATE_PROVIDER` defines the type of provider used by this instance. `local` and `jolimail` are the 2 possible option and `local` is the default.

When the provider `local` is used

- `TEMPLATE_ROOT` is the path where the templates will be loaded. In the container, the default is `/templates`.

When the provider `jolimail` is used

- `TEMPLATE_PROVIDER_JOLIMAIL_BASE_URL` is the base url where catapulte will fetch the templates. Something like `http://demo.jolimail.io`

## MRML relative

- `MRML_BREAKPOINT` is the number of pixels to use as breakpoint. It's the default MRML breakpoint size (`480px`).
- `MRML_KEEP_COMMENTS` is a flag defining if MRML should keep the comments.
- `MRML_SOCIAL_ICON_ORIGIN` is the base URL to load the social icons for `mj-social-element`. It's the default MRML value (`https://www.mailjet.com/images/theme/v1/icons/ico-social/`).

## SMTP relative

- `SMTP_HOSTNAME` is the hostname of the SMTP server (default `localhost`)
- `SMTP_PORT` is the port of the SMTP server (default `25`)
- `SMTP_USERNAME` is the username used to authenticate with the SMTP server
- `SMTP_PASSWORD` is the password used to authenticate with the SMTP server
- `SMTP_MAX_POOL_SIZE` is the max number of connection to the SMTP server (default `10`)
- `SMTP_TLS_ENABLED` enables TLS secure connection to the SMTP server (default `false`)
