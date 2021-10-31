use std::sync::Once;

use url::Url;

use crate::link_extractor::PageInfo;
use crate::setup_logging;

#[derive(Default, Debug)]
pub struct PageInfoBuilder {
    info: PageInfo,
}

fn build_links<const N: usize>(existing: &mut Vec<Url>, new: [&str; N]) {
    existing.extend(new.iter().map(|a| Url::parse(a).unwrap()));
}

impl PageInfoBuilder {
    pub fn new() -> Self {
        PageInfoBuilder::default()
    }
    pub fn build(self) -> PageInfo {
        self.info
    }
    pub fn external_links<const N: usize>(mut self, urls: [&str; N]) -> Self {
        build_links(&mut self.info.external_links, urls);
        self
    }
    pub fn internal_links<const N: usize>(mut self, urls: [&str; N]) -> Self {
        build_links(&mut self.info.internal_links, urls);
        self
    }
}

static INIT: Once = Once::new();

pub fn leak_setup_logging() {
    INIT.call_once(|| {
        Box::leak(Box::new(setup_logging()));
    })
}
