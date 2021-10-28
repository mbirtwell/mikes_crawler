use anyhow::anyhow;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    TokenizerResult,
};
use rocket::serde::ser::SerializeSeq;
use rocket::serde::{Serialize, Serializer};
use schemars::JsonSchema;
use url::Url;

#[derive(Clone, Default, Debug, Eq, PartialEq, Serialize, JsonSchema)]
pub struct PageInfo {
    #[serde(serialize_with = "serialize_vec_url")]
    #[schemars(with = "Vec<String>")]
    internal_links: Vec<Url>,
    #[serde(serialize_with = "serialize_vec_url")]
    #[schemars(with = "Vec<String>")]
    external_links: Vec<Url>,
}

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
            .ok_or(anyhow!("No href"))?;
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
                    // TODO: Logging
                    println!("No url for anchor: {}", e);
                }
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

pub fn parse_page(url: &Url, page: &str) -> PageInfo {
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
    if !buffers.is_empty() && matches!(result, TokenizerResult::Done) {
        panic!("TODO return error");
    }
    tokenizer.end();
    tokenizer.sink.output
}

#[cfg(test)]
mod tests {
    use crate::link_extractor::{parse_page, PageInfo};
    use indoc::indoc;
    use url::Url;

    #[derive(Default, Debug)]
    struct PageInfoBuilder {
        info: PageInfo,
    }

    fn build_links<const N: usize>(existing: &mut Vec<Url>, new: [&str; N]) {
        existing.extend(new.iter().map(|a| Url::parse(a).unwrap()));
    }

    impl PageInfoBuilder {
        fn new() -> Self {
            PageInfoBuilder::default()
        }
        fn build(self) -> PageInfo {
            self.info
        }
        fn external_links<const N: usize>(mut self, urls: [&str; N]) -> Self {
            build_links(&mut self.info.external_links, urls);
            self
        }
        fn internal_links<const N: usize>(mut self, urls: [&str; N]) -> Self {
            build_links(&mut self.info.internal_links, urls);
            self
        }
    }

    fn run_parse_successfully(html: &str) -> PageInfo {
        parse_page(&Url::parse("https://example.com/start").unwrap(), html)
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
