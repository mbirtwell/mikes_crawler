use anyhow::{anyhow, Error};
use rocket::http::Status;
use url::Url;

use reqwest::header::CONTENT_TYPE;
use reqwest::redirect::Policy;
use reqwest::StatusCode;
use rocket::async_trait;

pub const USER_AGENT: &str = "MikesCrawler";

#[derive(Clone)]
pub enum HttpResponse {
    Html(String),
    OtherContent(String),
    Redirect(Status, Url),
    ServerFailure(Status, String),
}

/// The HttpClient trait is just here to provide a level of indirection so that
/// we can isolate the rest of the code from the network for testing. The
/// production code that sits behind this is a thin layer over the reqwest http
/// client and as such needs a real http server to be available to test. So that
/// is left for the integration tests and there are no unit tests in this
/// module.
///
#[async_trait]
pub trait HttpClient: Sync + Send {
    async fn get(&self, url: Url) -> Result<HttpResponse, anyhow::Error>;
    async fn get_robots(&self, url: Url) -> Result<Option<String>, anyhow::Error>;

    // We want HttpClient to be cloneable because that maps best with how
    // reqwest::Client is intended to be user. reqwest::Client has an internal Arc.
    // But we can't make Clone a supertrait of HttpClient, because then it's not
    // object safe. I think that I also could have solved this by making my
    // holder for the http client Box<dyn HttpClient + Clone> every where, but
    // putting this inside HttpClient seems to capture that HttpClient has a
    // single user that requires this better.
    fn clone(&self) -> Box<dyn HttpClient>;
}

#[derive(Clone)]
pub struct ProdHttpClient {
    client: reqwest::Client,
}

impl ProdHttpClient {
    pub fn new() -> Self {
        ProdHttpClient {
            client: reqwest::ClientBuilder::new()
                .user_agent(USER_AGENT)
                // The crawler wants to handle redirects explicitly so that they
                // can be recorded.
                .redirect(Policy::none())
                .build()
                // No point trying to handle this error, we can't do anything
                // if we can't make HTTP requests
                .expect("Failed to startup. Couldn't create http client"),
        }
    }
}
#[async_trait]
impl HttpClient for ProdHttpClient {
    async fn get(&self, url: Url) -> Result<HttpResponse, anyhow::Error> {
        let response = self.client.get(url.clone()).send().await?;
        if response.status().is_success() {
            let content_type: mime::Mime = response
                .headers()
                .get(CONTENT_TYPE)
                .ok_or_else(|| anyhow!("No content type on OK response"))?
                .to_str()?
                .parse()?;
            if content_type.essence_str() == "text/html" {
                Ok(HttpResponse::Html(response.text().await?))
            } else {
                Ok(HttpResponse::OtherContent(content_type.to_string()))
            }
        } else if response.status().is_redirection() {
            let location = response
                .headers()
                .get("Location")
                .ok_or_else(|| anyhow!("No Location header on redirect"))?;
            let location = url.join(location.to_str()?)?;
            Ok(HttpResponse::Redirect(
                Status::new(response.status().as_u16()),
                location,
            ))
        } else {
            Ok(HttpResponse::ServerFailure(
                Status::new(response.status().as_u16()),
                response.text().await?,
            ))
        }
    }

    async fn get_robots(&self, url: Url) -> Result<Option<String>, Error> {
        let response = self.client.get(url.clone()).send().await?;
        if response.status().is_success() {
            Ok(Some(response.text().await?))
        } else if response.status() == StatusCode::NOT_FOUND {
            Ok(None)
        } else {
            anyhow::bail!("Got status {} for robots.txt", response.status())
        }
    }

    fn clone(&self) -> Box<dyn HttpClient> {
        Box::new(Clone::clone(self))
    }
}
