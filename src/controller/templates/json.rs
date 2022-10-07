use crate::error::ServerError;
use crate::service::provider::TemplateProvider;
use crate::service::smtp::SmtpPool;
use crate::service::template::TemplateOptions;
use actix_web::{guard::GuardContext, web, HttpResponse};
use lettre::Transport;
use mrml::prelude::render::Options as RenderOptions;
use serde::Deserialize;
use serde_json::Value as JsonValue;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Recipient {
    One(String),
    More(Vec<String>),
}

impl Recipient {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            Recipient::One(item) => vec![item.clone()],
            Recipient::More(list) => list.clone(),
        }
    }
}

impl Recipient {
    pub fn option_to_vec(item: &Option<Recipient>) -> Vec<String> {
        if let Some(item) = item {
            item.to_vec()
        } else {
            vec![]
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Payload {
    to: Option<Recipient>,
    cc: Option<Recipient>,
    bcc: Option<Recipient>,
    from: String,
    params: JsonValue,
}

impl Payload {
    fn to_options(&self) -> TemplateOptions {
        let to = Recipient::option_to_vec(&self.to);
        let cc = Recipient::option_to_vec(&self.cc);
        let bcc = Recipient::option_to_vec(&self.bcc);
        TemplateOptions::new(self.from.clone(), to, cc, bcc, self.params.clone(), vec![])
    }
}

pub fn filter(ctx: &GuardContext) -> bool {
    ctx.head()
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == "application/json")
        .unwrap_or(false)
}

pub async fn handler(
    render_opts: web::Data<RenderOptions>,
    smtp_pool: web::Data<SmtpPool>,
    template_provider: web::Data<TemplateProvider>,
    name: web::Path<String>,
    body: web::Json<Payload>,
) -> Result<HttpResponse, ServerError> {
    let template = template_provider.find_by_name(name.as_str()).await?;
    let options: TemplateOptions = body.to_options();
    options.validate()?;
    let email = template.to_email(&options, render_opts.as_ref())?;
    smtp_pool.send(&email)?;
    Ok(HttpResponse::NoContent().finish())
}

// LCOV_EXCL_START
#[cfg(test)]
mod tests {
    use crate::tests::{create_email, get_latest_inbox, ServerBuilder};
    use actix_web::http::StatusCode;
    use actix_web::test;
    use env_test_util::TempEnvVar;
    use serde_json::json;
    use std::thread::sleep;
    use std::time::Duration;

    #[actix_rt::test]
    #[serial]
    async fn success() {
        let _swagger = TempEnvVar::new("SWAGGER_ENABLED").with("true");
        let from = create_email();
        let to = create_email();
        let payload = json!({
            "from": from.clone(),
            "to": to.clone(),
            "params": {
                "name": "bob",
                "token": "this_is_a_token"
            }
        });
        let req = test::TestRequest::post()
            .uri("/templates/user-login")
            .set_json(&payload)
            .to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let list = get_latest_inbox(&from, &to).await;
        assert!(!list.is_empty());
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello bob!"));
        assert!(last.html.contains("Hello bob!"));
        assert!(last
            .html
            .contains("\"http://example.com/login?token=this_is_a_token\""));
    }

    #[actix_rt::test]
    #[serial]
    async fn success_ssl() {
        let from = create_email();
        let to = create_email();
        let payload = json!({
            "from": from.clone(),
            "to": to.clone(),
            "params": {
                "name": "bob",
                "token": "this_is_a_token"
            }
        });
        let req = test::TestRequest::post()
            .uri("/templates/user-login")
            .set_json(&payload)
            .to_request();
        let res = ServerBuilder::default()
            .secure(true)
            .invalid_cert(true)
            .execute(req)
            .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        for _ in 0..10 {
            sleep(Duration::from_secs(1));
            let list = get_latest_inbox(&from, &to).await;
            if !list.is_empty() {
                break;
            }
        }
        let list = get_latest_inbox(&from, &to).await;
        assert!(!list.is_empty());
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello bob!"));
        assert!(last.html.contains("Hello bob!"));
        assert!(last
            .html
            .contains("\"http://example.com/login?token=this_is_a_token\""));
    }

    #[actix_rt::test]
    #[serial]
    async fn failure_anonymous() {
        let from = create_email();
        let to = create_email();
        let payload = json!({
            "from": from.clone(),
            "to": to.clone(),
            "params": {
                "name": "bob",
                "token": "this_is_a_token"
            }
        });
        let req = test::TestRequest::post()
            .uri("/templates/user-login")
            .set_json(&payload)
            .to_request();
        let res = ServerBuilder::default()
            .authenticated(true)
            .execute(req)
            .await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_rt::test]
    #[serial]
    async fn failure_invalid_token() {
        let from = create_email();
        let to = create_email();
        let payload = json!({
            "from": from.clone(),
            "to": to.clone(),
            "params": {
                "name": "bob",
                "token": "this_is_a_token"
            }
        });
        let req = test::TestRequest::post()
            .uri("/templates/user-login")
            .append_header(("authorization", "Bearer hello-world"))
            .set_json(&payload)
            .to_request();
        let res = ServerBuilder::default()
            .authenticated(true)
            .execute(req)
            .await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_rt::test]
    #[serial]
    async fn success_authenticated() {
        let token = crate::service::jsonwebtoken::tests::create_token();
        let from = create_email();
        let to = create_email();
        let payload = json!({
            "from": from.clone(),
            "to": to.clone(),
            "params": {
                "name": "bob",
                "token": "this_is_a_token"
            }
        });
        let req = test::TestRequest::post()
            .uri("/templates/user-login")
            .append_header(("authorization", format!("Bearer {}", token)))
            .set_json(&payload)
            .to_request();
        let res = ServerBuilder::default()
            .authenticated(true)
            .execute(req)
            .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let list = get_latest_inbox(&from, &to).await;
        assert!(!list.is_empty());
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello bob!"));
        assert!(last.html.contains("Hello bob!"));
        assert!(last
            .html
            .contains("\"http://example.com/login?token=this_is_a_token\""));
    }

    #[actix_rt::test]
    #[serial]
    async fn success_even_missing_params() {
        let from = create_email();
        let to = create_email();
        let payload = json!({
            "from": from.clone(),
            "to": to.clone(),
            "params": {
                "name": "bob"
            }
        });

        let req = test::TestRequest::post()
            .uri("/templates/user-login")
            .set_json(&payload)
            .to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let list = get_latest_inbox(&from, &to).await;
        assert!(!list.is_empty());
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello bob!"));
        assert!(last.html.contains("Hello bob!"));
        assert!(last.html.contains("\"http://example.com/login?token=\""));
    }

    #[actix_rt::test]
    #[serial]
    async fn success_multiple_recipients() {
        let from = create_email();
        let to = vec![create_email(), create_email()];
        let cc = vec![create_email(), create_email()];
        let bcc = vec![create_email(), create_email()];
        let payload = json!({
            "from": from.clone(),
            "to": to.clone(),
            "cc": cc.clone(),
            "bcc": bcc.clone(),
            "params": {
                "name": "bob"
            }
        });
        let req = test::TestRequest::post()
            .uri("/templates/user-login")
            .set_json(&payload)
            .to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let list = get_latest_inbox(&from, &to[0]).await;
        assert!(!list.is_empty());
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello bob!"));
        assert!(last.html.contains("Hello bob!"));
        assert!(last.html.contains("\"http://example.com/login?token=\""));
    }

    #[actix_rt::test]
    #[serial]
    async fn failure_template_not_found() {
        let from = create_email();
        let to = create_email();
        let payload = json!({
            "from": from,
            "to": to,
            "params": {
                "name": "bob",
                "token": "this_is_a_token"
            }
        });
        let req = test::TestRequest::post()
            .uri("/templates/not-found")
            .set_json(&payload)
            .to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[actix_rt::test]
    #[serial]
    async fn failure_invalid_arguments() {
        let from = create_email();
        let payload = json!({
            "from": from,
            "params": {
                "name": "bob",
                "token": "this_is_a_token"
            }
        });
        let req = test::TestRequest::post()
            .uri("/templates/user-login")
            .set_json(&payload)
            .to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}
// LCOV_EXCL_END
