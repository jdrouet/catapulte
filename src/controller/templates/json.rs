use crate::error::ServerError;
use crate::service::provider::TemplateProvider;
use crate::service::smtp::SmtpPool;
use crate::service::template::TemplateOptions;
use axum::extract::{Extension, Json, Path};
use axum::http::StatusCode;
use lettre::Transport;
use mrml::prelude::render::Options as RenderOptions;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Recipient {
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
pub(crate) struct Payload {
    pub to: Option<Recipient>,
    pub cc: Option<Recipient>,
    pub bcc: Option<Recipient>,
    pub from: String,
    pub params: JsonValue,
}

impl Payload {
    fn to_options(&self) -> TemplateOptions {
        let to = Recipient::option_to_vec(&self.to);
        let cc = Recipient::option_to_vec(&self.cc);
        let bcc = Recipient::option_to_vec(&self.bcc);
        TemplateOptions::new(
            self.from.clone(),
            to,
            cc,
            bcc,
            self.params.clone(),
            Default::default(),
        )
    }
}

pub(crate) async fn handler(
    Extension(render_opts): Extension<Arc<RenderOptions>>,
    Extension(smtp_pool): Extension<SmtpPool>,
    Extension(template_provider): Extension<Arc<TemplateProvider>>,
    Path(name): Path<String>,
    Json(body): Json<Payload>,
) -> Result<StatusCode, ServerError> {
    let template = template_provider.find_by_name(name.as_str()).await?;
    let options: TemplateOptions = body.to_options();
    options.validate()?;
    let email = template.to_email(&options, &render_opts)?;
    smtp_pool.send(&email)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::{handler, Payload, Recipient};
    use crate::tests::{create_email, get_latest_inbox};
    use axum::extract::{Extension, Json, Path};
    use axum::http::StatusCode;
    use std::sync::Arc;
    use std::thread::sleep;
    use std::time::Duration;

    fn create_payload(from: &str, to: &str, token: &str) -> Payload {
        Payload {
            to: Some(Recipient::One(to.to_owned())),
            from: from.to_owned(),
            cc: None,
            bcc: None,
            params: serde_json::json!({
                "name": "bob",
                "token": token,
            }),
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn success() {
        crate::try_init_logs();
        let render_options = Arc::new(crate::service::render::Configuration::default().build());
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let template_provider =
            Arc::new(crate::service::provider::Configuration::default().build());

        let from = create_email();
        let to = create_email();
        let payload = create_payload(&from, &to, "this_is_a_token");

        let result = handler(
            Extension(render_options),
            Extension(smtp_pool),
            Extension(template_provider),
            Path("user-login".into()),
            Json(payload),
        )
        .await
        .unwrap();
        assert_eq!(result, StatusCode::NO_CONTENT);
        let list = get_latest_inbox(&from, &to).await;
        assert!(!list.is_empty());
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello bob!"));
        assert!(last.html.contains("Hello bob!"));
        assert!(last
            .html
            .contains("\"http://example.com/login?token=this_is_a_token\""));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn success_ssl() {
        crate::try_init_logs();
        let render_options = Arc::new(crate::service::render::Configuration::default().build());
        let smtp_pool = crate::service::smtp::Configuration::secure()
            .build()
            .unwrap();
        let template_provider =
            Arc::new(crate::service::provider::Configuration::default().build());

        let from = create_email();
        let to = create_email();
        let payload = create_payload(&from, &to, "this_is_a_secure_token");

        let result = handler(
            Extension(render_options),
            Extension(smtp_pool),
            Extension(template_provider),
            Path("user-login".into()),
            Json(payload),
        )
        .await
        .unwrap();
        assert_eq!(result, StatusCode::NO_CONTENT);
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
            .contains("\"http://example.com/login?token=this_is_a_secure_token\""));
    }

    // #[tokio::test]
    // #[serial_test::serial]
    // async fn failure_anonymous() {
    //     let from = create_email();
    //     let to = create_email();
    //     let payload = json!({
    //         "from": from.clone(),
    //         "to": to.clone(),
    //         "params": {
    //             "name": "bob",
    //             "token": "this_is_a_token"
    //         }
    //     });
    //     let req = test::TestRequest::post()
    //         .uri("/templates/user-login")
    //         .set_json(&payload)
    //         .to_request();
    //     let res = ServerBuilder::default()
    //         .authenticated(true)
    //         .execute(req)
    //         .await;
    //     assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    // }

    //     #[actix_rt::test]
    //     #[serial]
    //     async fn failure_invalid_token() {
    //         let from = create_email();
    //         let to = create_email();
    //         let payload = json!({
    //             "from": from.clone(),
    //             "to": to.clone(),
    //             "params": {
    //                 "name": "bob",
    //                 "token": "this_is_a_token"
    //             }
    //         });
    //         let req = test::TestRequest::post()
    //             .uri("/templates/user-login")
    //             .append_header(("authorization", "Bearer hello-world"))
    //             .set_json(&payload)
    //             .to_request();
    //         let res = ServerBuilder::default()
    //             .authenticated(true)
    //             .execute(req)
    //             .await;
    //         assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    //     }

    //     #[actix_rt::test]
    //     #[serial]
    //     async fn success_authenticated() {
    //         let token = crate::service::jsonwebtoken::tests::create_token();
    //         let from = create_email();
    //         let to = create_email();
    //         let payload = json!({
    //             "from": from.clone(),
    //             "to": to.clone(),
    //             "params": {
    //                 "name": "bob",
    //                 "token": "this_is_a_token"
    //             }
    //         });
    //         let req = test::TestRequest::post()
    //             .uri("/templates/user-login")
    //             .append_header(("authorization", format!("Bearer {}", token)))
    //             .set_json(&payload)
    //             .to_request();
    //         let res = ServerBuilder::default()
    //             .authenticated(true)
    //             .execute(req)
    //             .await;
    //         assert_eq!(res.status(), StatusCode::NO_CONTENT);
    //         let list = get_latest_inbox(&from, &to).await;
    //         assert!(!list.is_empty());
    //         let last = list.first().unwrap();
    //         assert!(last.text.contains("Hello bob!"));
    //         assert!(last.html.contains("Hello bob!"));
    //         assert!(last
    //             .html
    //             .contains("\"http://example.com/login?token=this_is_a_token\""));
    //     }

    #[tokio::test]
    #[serial_test::serial]
    async fn success_even_missing_params() {
        let render_options = Arc::new(crate::service::render::Configuration::default().build());
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let template_provider =
            Arc::new(crate::service::provider::Configuration::default().build());

        let from = create_email();
        let to = create_email();
        let mut payload = create_payload(&from, &to, "this_is_a_secure_token");
        payload.params = serde_json::json!({ "name": "Alice" });

        let result = handler(
            Extension(render_options),
            Extension(smtp_pool),
            Extension(template_provider),
            Path("user-login".into()),
            Json(payload),
        )
        .await
        .unwrap();
        assert_eq!(result, StatusCode::NO_CONTENT);
        let list = get_latest_inbox(&from, &to).await;
        assert!(!list.is_empty());
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello Alice!"));
        assert!(last.html.contains("Hello Alice!"));
        assert!(last.html.contains("\"http://example.com/login?token=\""));
    }

    //     #[actix_rt::test]
    //     #[serial]
    //     async fn success_multiple_recipients() {
    //         let from = create_email();
    //         let to = vec![create_email(), create_email()];
    //         let cc = vec![create_email(), create_email()];
    //         let bcc = vec![create_email(), create_email()];
    //         let payload = json!({
    //             "from": from.clone(),
    //             "to": to.clone(),
    //             "cc": cc.clone(),
    //             "bcc": bcc.clone(),
    //             "params": {
    //                 "name": "bob"
    //             }
    //         });
    //         let req = test::TestRequest::post()
    //             .uri("/templates/user-login")
    //             .set_json(&payload)
    //             .to_request();
    //         let res = ServerBuilder::default().execute(req).await;
    //         assert_eq!(res.status(), StatusCode::NO_CONTENT);
    //         let list = get_latest_inbox(&from, &to[0]).await;
    //         assert!(!list.is_empty());
    //         let last = list.first().unwrap();
    //         assert!(last.text.contains("Hello bob!"));
    //         assert!(last.html.contains("Hello bob!"));
    //         assert!(last.html.contains("\"http://example.com/login?token=\""));
    //     }

    //     #[actix_rt::test]
    //     #[serial]
    //     async fn failure_template_not_found() {
    //         let from = create_email();
    //         let to = create_email();
    //         let payload = json!({
    //             "from": from,
    //             "to": to,
    //             "params": {
    //                 "name": "bob",
    //                 "token": "this_is_a_token"
    //             }
    //         });
    //         let req = test::TestRequest::post()
    //             .uri("/templates/not-found")
    //             .set_json(&payload)
    //             .to_request();
    //         let res = ServerBuilder::default().execute(req).await;
    //         assert_eq!(res.status(), StatusCode::NOT_FOUND);
    //     }

    //     #[actix_rt::test]
    //     #[serial]
    //     async fn failure_invalid_arguments() {
    //         let from = create_email();
    //         let payload = json!({
    //             "from": from,
    //             "params": {
    //                 "name": "bob",
    //                 "token": "this_is_a_token"
    //             }
    //         });
    //         let req = test::TestRequest::post()
    //             .uri("/templates/user-login")
    //             .set_json(&payload)
    //             .to_request();
    //         let res = ServerBuilder::default().execute(req).await;
    //         assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    //     }
}
