use httpmock::MockServer;
use indoc::indoc;
use rocket::serde::json::Value;
use rocket::tokio;

use mikes_crawler::setup_logging;

#[test]
fn simple() {
    let _slog_guard = setup_logging();

    let mock_server = MockServer::start();
    let ms = mock_server.mock(|when, then| {
        when.path("/start");
        then.status(200).body(indoc! {r#"
            <!DOCTYPE html>
            <html>
                <head></head>
                <body>
                    <a href="https://notexample.com/another">Interesting</a>
                </body>
            </html>
        "#});
    });

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.spawn(mikes_crawler::run_server());

    let start = mock_server.url("/start");

    let response = reqwest::blocking::get(format!(
        "http://127.0.0.1:{}/crawl/{}",
        rocket::Config::default().port,
        urlencoding::encode(&start)
    ))
    .unwrap();

    assert_eq!(response.status(), 200);
    let body: Value = response.json().unwrap();
    assert_eq!(
        body["pages"][start]["Crawled"]["external_links"][0],
        "https://notexample.com/another"
    );
    ms.assert();
}
