use rocket::http::Status;
use rocket::serde::ser::SerializeSeq;
use rocket::serde::Serializer;
use url::Url;

pub fn serialize_status<S>(data: &Status, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_u16(data.code)
}

pub fn serialize_url<S>(data: &Url, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(data.as_str())
}

#[allow(clippy::ptr_arg)]
pub fn serialize_vec_url<S>(data: &Vec<Url>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = serializer.serialize_seq(Some(data.len()))?;
    for url in data {
        seq.serialize_element(url.as_str())?;
    }
    seq.end()
}
