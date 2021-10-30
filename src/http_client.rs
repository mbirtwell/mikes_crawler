use anyhow::anyhow;
use rocket::http::Status;
use url::Url;

use reqwest::redirect::Policy;
use rocket::async_trait;

#[derive(Clone)]
pub enum HttpResponse {
    Ok(String),
    #[allow(dead_code)]
    Redirect(Status, Url),
    ServerFailure(Status, String),
}

#[async_trait]
pub trait HttpClient: Sync + Send {
    async fn get(&self, url: Url) -> Result<HttpResponse, anyhow::Error>;
}

pub struct ProdHttpClient {
    client: reqwest::Client,
}

impl ProdHttpClient {
    pub fn new() -> Self {
        ProdHttpClient {
            client: reqwest::ClientBuilder::new()
                .user_agent("MikesCrawler")
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
            Ok(HttpResponse::Ok(response.text().await?))
        } else if response.status().is_redirection() {
            let location = response
                .headers()
                .get("Location")
                .ok_or(anyhow!("No Location header on redirect"))?;
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
}
