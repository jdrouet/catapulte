use crate::error::ServerError;
use crate::service::jsonwebtoken::{Claims, Decoder};
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::Error as ActixError;
use futures::future::{ok, Ready};
use futures::Future;
use std::env;
use std::pin::Pin;
use std::task::{Context, Poll};

#[derive(Debug)]
pub struct Authentication {
    decoder: Decoder,
    enabled: bool,
    header: String,
}

impl Authentication {
    pub fn from_env() -> Self {
        let enabled = env::var("AUTHENTICATION_ENABLED")
            .map(|value| value == "true")
            .unwrap_or(false);
        if !enabled {
            log::warn!("authentication middleware disabled");
        }
        Self {
            decoder: Decoder::from_env().expect("couldn't build jsonwebtoken decoder"),
            enabled,
            header: env::var("AUTHENTICATION_HEADER").unwrap_or_else(|_| "Authorization".into()),
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
            enabled: self.enabled,
            header: self.header.clone(),
            service,
        })
    }
}

pub struct AuthenticationMiddleware<S> {
    decoder: Decoder,
    enabled: bool,
    header: String,
    service: S,
}

impl<S> AuthenticationMiddleware<S> {
    fn get_token<'req>(&self, req: &'req ServiceRequest) -> Result<&'req str, String> {
        req.headers()
            .get(&self.header)
            .and_then(|header| header.to_str().ok())
            .map(|header| header.trim_start_matches("Bearer "))
            .ok_or_else(|| "no authorization token provided".to_string())
    }

    fn parse_token(&self, req: &ServiceRequest) -> Result<Claims, String> {
        self.get_token(req).and_then(|token| {
            self.decoder.decode(token).map_err(|error| {
                log::debug!("invalid token {:?}", error);
                "invalid authorization token".to_string()
            })
        })
    }
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
        if !self.enabled {
            let fut = self.service.call(req);
            return Box::pin(async move { fut.await });
        }
        match self.parse_token(&req) {
            Ok(_) => {
                let fut = self.service.call(req);
                Box::pin(async move { fut.await })
            }
            Err(msg) => {
                let inner_error = ServerError::Unauthorized(msg);
                Box::pin(async move { Ok(req.error_response(inner_error)) })
            }
        }
    }
}
