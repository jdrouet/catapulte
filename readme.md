# Catapulte

[![Build Status](https://travis-ci.com/jdrouet/catapulte.svg?branch=main)](https://travis-ci.com/jdrouet/catapulte)
[![codecov](https://codecov.io/gh/jdrouet/catapulte/branch/main/graph/badge.svg)](https://codecov.io/gh/jdrouet/catapulte)

[![Docker Pulls](https://img.shields.io/docker/pulls/jdrouet/catapulte)](https://hub.docker.com/r/jdrouet/catapulte)
[![Docker Image Size (latest by date)](https://img.shields.io/docker/image-size/jdrouet/catapulte?sort=date)](https://hub.docker.com/r/jdrouet/catapulte)

## What is catapulte?

Catapulte is an open source mailer you can host yourself.

You can use it to quickly catapult your transactionnal emails to destination.

[![Deploy](https://www.herokucdn.com/deploy/button.svg)](https://heroku.com/deploy?template=https://github.com/jdrouet/catapulte)

## Why did we build catapulte?

Catapulte comes from the frustration of using several email providers.
We used to work with products like [sendgrid](https://sendgrid.com/),
[mailgun](https://www.mailgun.com/), [mailchimp](https://mailchimp.com/), [sendinblue](https://www.sendinblue.com/), etc.

But they have many disadvantages :

- Most of them are not really transactionnal oriented, and users complain that their login emails take a long time to arrive.
- You cannot host it nor use it on premise
- It's American, with the patriot act, they are able to access your users data.
- They usually don't have templating tools for our non tech coworkers that ask us to change a wording every 2 days.
  And when they do, the editors are like html online editors, so it ends up being our job to make the template anyway.

## How to use it?

Catapulte is a simple service that renders your mjml template, interpolates the data and then sends it to a SMTP server.
If you want to see how to create your own template, take a look at the `/template` folder in this repository.

You then have several options for starting catapulte. We recommend using Docker if you are on a amd64, i386 or arm64v8 architecture.
By doing the following, you'll be able to have a running server that will render and send your email.

```bash
docker run -d \
  --name catapulte \
  -e SMTP_HOSTNAME=localhost \
  -e SMTP_PORT=25 \
  -e SMTP_USERNAME=optional \
  -e SMTP_PASSWORD=optional \
  -e SMTP_TLS_ENABLED=true \
  -e TEMPLATE_PROVIDER=local \
  -e TEMPLATE_ROOT=/templates \
  -p 3000:3000 \
  -v /path/to/your/templates:/templates:ro \
  jdrouet/catapulte:master
```

Once your server started, you can simply send an email using an `HTTP` request.

```bash
curl -X POST -v \
  -H "Content-Type: application/json" \
  --data '{"from":"alice@example.com","to":"bob@example.com","params":{"some":"data"}}' \
  http://localhost:3000/templates/the-name-of-your-template
```

You can also send attachments using a multipart request.

```bash
curl -X POST -v \
  -F attachments=@asset/cat.jpg \
  -F from=alice@example.com \
  -F to=bob@example.com \
  -F params='{"some":"data"}' \
  http://localhost:3000/templates/user-login
```

You can configure it with [some environment variable](./wiki/environment-variables.md) and can find more information in [this wiki](./wiki/template-provider.md).

If you some API specification, the [Open API specification](./openapi.yml) is also available.

To use it in production, we prepared a documentation on how to use Catapulte with [Amazon Simple Email Service](./wiki/with-aws-ses.md).

### Sending to multiple recipients
You can send the same email to multiple recipients just by using an array in the `to` field, like this:

```bash
curl -X POST -v \
  -H "Content-Type: application/json" \
  --data '{"from":"alice@example.com","to":["bob@example.com","jon@example.com"],"params":{"some":"data"}}' \
  http://localhost:3000/templates/the-name-of-your-template
```

## Should you use it?

If, like us, you didn't find any good way of doing transactionnal emails, then YES!

## Why you should use it :

- You work in a startup

  - You don't have shit loads of money to spend on the mailing tools, so use something opensource, send your emails from your own SMTP (or from Amazon SES, it's cheap)
  - You don't have time to change the email template everyday, so let your Product Owner do it
  - You wanna be able to add this little feature, just do a pull request...

- You work in a big company

  - You cannot use those external services because you're not allowed to put your user's data on an external service.
  - You cannot access external services because it's blocked by the proxy
  - You want to customise the way you authenticate to your SMTP
  - You want something user friendly enough that your manager can write the emails

## Thank you!

<a href="https://www.buymeacoffee.com/jdrouet" target="_blank"><img src="https://cdn.buymeacoffee.com/buttons/v2/default-blue.png" alt="Buy Me A Coffee" style="height: 60px !important;width: 217px !important;" ></a>
