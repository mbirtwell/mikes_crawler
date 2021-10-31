use std::sync::Once;

use httpmock::MockServer;
use indoc::indoc;
use rocket::serde::json::Value;
use rocket::tokio;

use mikes_crawler::setup_logging;

static INIT: Once = Once::new();

fn setup() {
    INIT.call_once(|| {
        Box::leak(Box::new(setup_logging()));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.spawn(mikes_crawler::run_server());
        Box::leak(Box::new(runtime));
    })
}

#[test]
fn simple() {
    setup();

    let mock_server = MockServer::start();
    let ms = mock_server.mock(|when, then| {
        when.path("/start");
        then.status(200)
            .header("Content-Type", "text/html")
            .body(indoc! {r#"
            <!DOCTYPE html>
            <html>
                <head></head>
                <body>
                    <a href="https://notexample.com/another">Interesting</a>
                </body>
            </html>
        "#});
    });

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

#[test]
fn relative_redirects() {
    setup();

    let mock_server = MockServer::start();
    let ms = mock_server.mock(|when, then| {
        when.path("/start");
        then.status(301).header("Location", "/next");
    });

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
        body["pages"][start]["Redirect"]["location"],
        mock_server.url("/next")
    );
    ms.assert();
}

#[test]
fn ignore_non_html() {
    setup();

    let mock_server = MockServer::start();
    let ms = mock_server.mock(|when, then| {
        when.path("/start");
        then.status(200)
            .header("Content-Type", "x-application/something")
            .body("XXXX");
    });

    let start = mock_server.url("/start");

    let response = reqwest::blocking::get(format!(
        "http://127.0.0.1:{}/crawl/{}",
        rocket::Config::default().port,
        urlencoding::encode(&start)
    ))
    .unwrap();

    assert_eq!(response.status(), 200);
    let body: Value = response.json().unwrap();
    println!("{:?}", body);
    assert_eq!(
        body["pages"][start]["OtherContent"],
        "x-application/something"
    );
    ms.assert();
}

#[test]
fn ignores_things_excluded_by_robots() {
    setup();

    let mock_server = MockServer::start();
    let _mstart = mock_server.mock(|when, then| {
        when.path("/start");
        then.status(200)
            .header("Content-Type", "text/html")
            .body(format!(
                indoc! {r#"
                    <!DOCTYPE html>
                    <html>
                        <head></head>
                        <body>
                            <a href="https://notexample.com/another">Interesting</a>
                            <a href="{}">Interesting</a>
                        </body>
                    </html>
                "#},
                mock_server.url("/disallowed"),
            ));
    });
    let mrobots = mock_server.mock(|when, then| {
        when.path("/robots.txt");
        then.status(200)
            .header("Content-Type", "text/plain")
            .body(indoc! {r#"
                User-agent: *
                Disallow: /disallowed
            "#});
    });
    let mdisallowed = mock_server.mock(|when, then| {
        when.path("/disallowed");
        then.status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(format!(
                indoc! {r#"
                    <!DOCTYPE html>
                    <html>
                        <head></head>
                        <body>
                            <a href="{}">Interesting</a>
                        </body>
                    </html>
                "#},
                mock_server.url("/hidden")
            ));
    });
    let mhidden = mock_server.mock(|when, then| {
        when.path("/hidden");
        then.status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .body("");
    });

    let start = mock_server.url("/start");

    let response = reqwest::blocking::get(format!(
        "http://127.0.0.1:{}/crawl/{}",
        rocket::Config::default().port,
        urlencoding::encode(&start)
    ))
    .unwrap();

    assert_eq!(response.status(), 200);
    let body: Value = response.json().unwrap();
    println!("{:?}", body);

    assert_eq!(
        body["pages"][mock_server.url("/disallowed")]
            .as_str()
            .unwrap(),
        "ExcludedByRobotsTxt"
    );

    mrobots.assert_hits(1);
    mdisallowed.assert_hits(0);
    mhidden.assert_hits(0)
}

fn str_array(v: &Value) -> Vec<&str> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect::<Vec<_>>()
}

#[test]
fn collected_things() {
    setup();

    let mock_server = MockServer::start();
    let mstart = mock_server.mock(|when, then| {
        when.path("/start");
        then.status(200)
            .header("Content-Type", "text/html")
            .body(format!(
                indoc! {r#"
                    <!DOCTYPE html>
                    <html>
                        <head></head>
                        <body>
                            <a href="https://notexample.com/another">Interesting</a>
                            <a href="{}">Interesting</a>
                            <a href="{}">Interesting</a>
                            <a href="{}">Interesting</a>
                            <a href="{}">Interesting</a>
                            <a href="/relative">Interesting</a>
                        </body>
                    </html>
                "#},
                mock_server.url("/another"),
                mock_server.url("/third"),
                mock_server.url("/pdf"),
                mock_server.url("/redirect"),
            ));
    });
    let manother = mock_server.mock(|when, then| {
        when.path("/another");
        then.status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(format!(
                indoc! {r#"
                    <!DOCTYPE html>
                    <html>
                        <head></head>
                        <body>
                            <a href="{}">Interesting</a>
                        </body>
                    </html>
                "#},
                mock_server.url("/third")
            ));
    });
    let mthird = mock_server.mock(|when, then| {
        when.path("/third");
        then.status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(indoc! {r#"
                    <!DOCTYPE html>
                    <html>
                        <head></head>
                        <body>
                            <a href="/relative">Interesting</a>
                        </body>
                    </html>
                "#});
    });
    let mpdf = mock_server.mock(|when, then| {
        when.path("/pdf");
        then.status(200)
            .header("Content-Type", "x-application/something")
            .body("XXXX");
    });
    let mredirect = mock_server.mock(|when, then| {
        when.path("/redirect");
        then.status(301)
            .header("Location", &mock_server.url("/start"));
    });
    let mrelative = mock_server.mock(|when, then| {
        when.path("/relative");
        then.status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(indoc! {r#"
                    <!DOCTYPE html>
                    <html>
                        <head></head>
                        <body>
                            <a href="/third">Interesting</a>
                        </body>
                    </html>
                "#});
    });

    let start = mock_server.url("/start");

    let response = reqwest::blocking::get(format!(
        "http://127.0.0.1:{}/crawl/{}",
        rocket::Config::default().port,
        urlencoding::encode(&start)
    ))
    .unwrap();

    assert_eq!(response.status(), 200);
    let body: Value = response.json().unwrap();
    println!("{:?}", body);
    assert_eq!(
        str_array(&body["pages"][&start]["Crawled"]["internal_links"]),
        vec![
            &mock_server.url("/another"),
            &mock_server.url("/third"),
            &mock_server.url("/pdf"),
            &mock_server.url("/redirect"),
            &mock_server.url("/relative")
        ]
    );
    assert_eq!(
        str_array(&body["pages"][&start]["Crawled"]["external_links"]),
        vec!["https://notexample.com/another"]
    );
    assert_eq!(
        str_array(&body["pages"][mock_server.url("/another")]["Crawled"]["internal_links"]),
        vec![&mock_server.url("/third")]
    );
    mstart.assert_hits(1);
    manother.assert_hits(1);
    mthird.assert_hits(1);
    mpdf.assert_hits(1);
    mredirect.assert_hits(1);
    mrelative.assert_hits(1);
}
