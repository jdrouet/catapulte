# Template providers

Catapulte comes with a mechanism of template. You can interpolate variables with the embedded [handlebars](https://crates.io/crates/handlebars) template engine used by catapulte under the hood.

But the templates can be fetched from a local folder, or from a dynamic provider, like [jolimail](https://github.com/jdrouet/jolimail).
And this can be specified by passing an environment variable `TEMPLATE_PROVIDER=local|jolimail`.

## Using the local provider

When you start catapulte, you have to provide the environment variable `TEMPLATE_PROVIDER=local`.

On top of that, you've to indicate where the templates will be located by using the environment variable `TEMPLATE_ROOT=/where/the/templates/are/located`.

When this is done, you've to add some templates in this folder. For each template, you have to create a folder in kebab case. That name will be used when specifying which template to use.

Then, in that folder, you have to use a `metadata.json` file containing the following attributes

```json
{
  "name": "the-folder-name",
  "description": "Whatever description you want to give",
  // name of the template file in that folder
  "path": "template.mjml",
  // json schema for the variables in the template
  "attributes": {
    "type": "object",
    "properties": {
      "name": {
        "type": "string"
      }
    },
    "required": ["name"]
  }
}
```

And now you can add the `mjml` template in a `template.mjml` file (you can change the name if you change it also in the `metadata.json`).

```xml
<mjml>
  <mj-head>
    <mj-title>Hello {{name}}!</mj-title>
    <mj-preview>Hello {{name}}!</mj-preview>
  </mj-head>
  <mj-body>
    <mj-section>
      <mj-column>
        <mj-text>Hello {{name}}!</mj-text>
      </mj-column>
    </mj-section>
  </mj-body>
</mjml>
```

## Using jolimail

When you start catapulte, you have to provide the environment variable `TEMPLATE_PROVIDER=jolimail`.

After that, you have to specify the url where your [jolimail](https://github.com/jdrouet/jolimail) instance is reachable with the environment variable `TEMPLATE_PROVIDER_JOLIMAIL_BASE_URL=http://my-instance`
.

And that's it. You can create your template directly in [jolimail](https://github.com/jdrouet/jolimail), create versions, and simply request them.
