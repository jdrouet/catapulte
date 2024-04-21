use clap::Parser;

mod action;

#[derive(Parser)]
#[clap(about, author, version)]
struct Arguments {
    /// Log level.
    #[clap(short, long, env, default_value = "catapulte=debug,tower_http=debug")]
    log: String,
    /// Disable color in logs.
    #[clap(long, env, default_value = "false")]
    disable_log_color: bool,
    #[command(subcommand)]
    action: action::Action,
}

impl Arguments {
    fn init_logs(&self) {
        catapulte::init_logs(&self.log, !self.disable_log_color).expect("couldn't init logger")
    }
}

#[tokio::main]
async fn main() {
    let args = Arguments::parse();
    args.init_logs();

    args.action.execute().await
}
