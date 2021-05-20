use crate::error::ServerError;
use crate::service::jsonwebtoken::Decoder;
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::Error as ActixError;
use futures::future::{ok, Ready};
use futures::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

#[derive(Debug)]
pub struct Authentication {
    decoder: Decoder,
    header: String,
}

impl Authentication {
    pub fn from_env() -> Self {
        Self {
            decoder: Decoder::from_env().expect("couldn't build jsonwebtoken decoder"),
            header: std::env::var("AUTHENTICATION_HEADER")
                .unwrap_or_else(|_| "Authorization".into()),
        }
    }
}

impl<S, B> Transform<S, ServiceRequest> for Authentication
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = ActixError>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = ActixError;
    type InitError = ();
    type Transform = AuthenticationMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(AuthenticationMiddleware {
            decoder: self.decoder.clone(),
            header: self.header.clone(),
            service,
        })
    }
}

pub struct AuthenticationMiddleware<S> {
    decoder: Decoder,
    header: String,
    service: S,
}

impl<S, B> Service<ServiceRequest> for AuthenticationMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = ActixError>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = ActixError;
    #[allow(clippy::type_complexity)]
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let token = req
            .headers()
            .get(&self.header)
            .and_then(|header| header.to_str().ok());
        if let Err(error) = self.decoder.decode(token) {
            log::debug!("unauthorized {:?}", error);
            let inner_error = ServerError::Unauthorized("invalid authorization token".into());
            Box::pin(async move { Ok(req.error_response(inner_error)) })
        } else {
            let fut = self.service.call(req);
            Box::pin(async move { fut.await })
        }
    }
}
