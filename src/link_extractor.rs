/// Parse html and return the links found in anchor elements.
use anyhow::anyhow;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    TokenizerResult,
};
use rocket::serde::ser::SerializeSeq;
use rocket::serde::{Serialize, Serializer};
use schemars::JsonSchema;
use slog_scope::info;
use url::Url;

#[derive(Clone, Default, Debug, Eq, PartialEq, Serialize, JsonSchema)]
pub struct PageInfo {
    #[serde(serialize_with = "serialize_vec_url")]
    #[schemars(with = "Vec<String>")]
    /// Links found on the page with the same domain as the page
    pub internal_links: Vec<Url>,
    #[serde(serialize_with = "serialize_vec_url")]
    #[schemars(with = "Vec<String>")]
    /// Links found on the page with different domains to the page
    pub external_links: Vec<Url>,
}

#[allow(clippy::ptr_arg)]
fn serialize_vec_url<S>(data: &Vec<Url>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = serializer.serialize_seq(Some(data.len()))?;
    for url in data {
        seq.serialize_element(url.as_str())?;
    }
    seq.end()
}

struct PageInfoSink<'a> {
    url: &'a Url,
    output: PageInfo,
}

impl<'a> PageInfoSink<'a> {
    fn process_anchor(&mut self, tag: Tag) -> anyhow::Result<()> {
        let href = tag
            .attrs
            .iter()
            .find(|a| &a.name.local == "href")
            .ok_or_else(|| anyhow!("No href"))?;
        let url = self.url.join(&href.value)?;
        if url.domain() == self.url.domain() {
            self.output.internal_links.push(url);
        } else {
            self.output.external_links.push(url);
        }
        Ok(())
    }
}

impl<'a> TokenSink for PageInfoSink<'a> {
    type Handle = ();

    fn process_token(&mut self, token: Token, _line_number: u64) -> TokenSinkResult<Self::Handle> {
        match token {
            Token::TagToken(tag) if tag.kind == TagKind::StartTag && &tag.name == "a" => {
                if let Err(e) = self.process_anchor(tag) {
                    info!("No url for anchor: {}", e);
                }
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

pub fn parse_page(url: &Url, page: &str) -> anyhow::Result<PageInfo> {
    // It would be nice to pass this function the body stream rather than the
    // buffered body to cut down on memory usage. But neither BufferQueue or
    // Tokenizer are Send or Sync so we can't hold them across await points
    // whilst we wait on the buffer stream. We can work around this is one of
    // two ways:
    //  * Package them up in some combination of Arc/Mutex then unwrap that at
    //    the end to get the PageInfo out
    //  * Push this off on two another thread that's running a tokio::LocalSet
    //
    // Neither of these seemed worth it given the cost of buffering it pretty
    // low.
    let mut buffers = BufferQueue::new();
    buffers.push_back(page.into());
    let mut tokenizer = Tokenizer::new(
        PageInfoSink {
            url,
            output: PageInfo::default(),
        },
        TokenizerOpts::default(),
    );
    let result = tokenizer.feed(&mut buffers);
    if !buffers.is_empty() || !matches!(result, TokenizerResult::Done) {
        // I don't think that this can ever happen.
        anyhow::bail!("parse_page didn't process all of input buffer");
    }
    tokenizer.end();
    Ok(tokenizer.sink.output)
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use url::Url;

    use crate::link_extractor::{parse_page, PageInfo};
    use crate::test_util::PageInfoBuilder;

    fn run_parse_successfully(html: &str) -> PageInfo {
        parse_page(&Url::parse("https://example.com/start").unwrap(), html).unwrap()
    }

    #[test]
    fn empty_lists_for_empty_html() {
        let html = indoc! {"
            <!DOCTYPE html>
            <html>
                <head></head>
                <body>
                    Boring!
                </body>
            </html
        "};

        let rv = run_parse_successfully(html);

        assert_eq!(rv, PageInfo::default())
    }

    #[test]
    fn extracts_links_in_domain_as_internal_link() {
        let html = indoc! {r##"
            <!DOCTYPE html>
            <html>
                <head></head>
                <body>
                    <a href="https://example.com/another">Interesting</a>
                </body>
            </html
        "##};

        let rv = run_parse_successfully(html);

        assert_eq!(
            rv,
            PageInfoBuilder::new()
                .internal_links(["https://example.com/another"])
                .build()
        )
    }

    #[test]
    fn continues_after_script_tags() {
        let html = indoc! {r##"
            <!DOCTYPE html>
            <html>
                <head></head>
                <body>
                    <script>
                        function get_string() {
                            return "Some JS here";
                        }
                        alert(get_string())
                    </script>
                    <a href="https://example.com/another">Interesting</a>
                </body>
            </html
        "##};

        let rv = run_parse_successfully(html);

        assert_eq!(
            rv,
            PageInfoBuilder::new()
                .internal_links(["https://example.com/another"])
                .build()
        )
    }

    #[test]
    fn extracts_links_in_other_domain_as_external_link() {
        let html = indoc! {r##"
            <!DOCTYPE html>
            <html>
                <head></head>
                <body>
                    <a href="https://notexample.com/another">Interesting</a>
                </body>
            </html
        "##};

        let rv = run_parse_successfully(html);

        assert_eq!(
            rv,
            PageInfoBuilder::new()
                .external_links(["https://notexample.com/another"])
                .build()
        )
    }

    #[test]
    fn relative_links_are_internal_links() {
        let html = indoc! {r##"
            <!DOCTYPE html>
            <html>
                <head></head>
                <body>
                    <a href="/another">Interesting</a>
                </body>
            </html
        "##};

        let rv = run_parse_successfully(html);

        assert_eq!(
            rv,
            PageInfoBuilder::new()
                .internal_links(["https://example.com/another"])
                .build()
        )
    }
}
