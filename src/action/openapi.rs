use crate::controller::swagger::ApiDoc;
use utoipa::OpenApi;

#[derive(clap::Parser)]
pub(crate) struct Action {
    /// Pretty prints the openapi definition.
    #[clap(short, long)]
    pub pretty: bool,
}

impl Action {
    pub(crate) fn execute(&self) {
        let api = ApiDoc::openapi();
        if self.pretty {
            println!("{}", api.to_pretty_json().unwrap());
        } else {
            println!("{}", api.to_json().unwrap());
        }
    }
}
