use mikes_crawler::{run_server, setup_logging};

#[rocket::main]
async fn main() {
    let _slog_guard = setup_logging();
    run_server().await;
}
