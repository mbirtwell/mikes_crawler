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

#[async_trait]
pub trait HttpClient: Sync + Send {
    async fn get(&self, url: Url) -> Result<HttpResponse, anyhow::Error>;
    async fn get_robots(&self, url: Url) -> Result<Option<String>, anyhow::Error>;
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
