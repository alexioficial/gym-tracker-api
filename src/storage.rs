//! Progress photos live in an S3-compatible bucket. The API never streams image
//! bytes: it hands out short-lived presigned URLs (AWS Signature V4, query-string
//! form) for the browser to upload and view, and deletes objects itself.

use std::{env, time::Duration};

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub const UPLOAD_URL_SECONDS: u64 = 10 * 60;
pub const VIEW_URL_SECONDS: u64 = 5 * 60;
pub const PHOTO_CONTENT_TYPES: [(&str, &str); 3] = [
    ("image/jpeg", "jpg"),
    ("image/png", "png"),
    ("image/webp", "webp"),
];

#[derive(Clone, Debug)]
pub struct S3Config {
    /// `https://s3.us-east-1.amazonaws.com`, an R2 or MinIO endpoint, etc.
    scheme: String,
    endpoint_host: String,
    region: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
    /// R2 and MinIO usually want `endpoint/bucket/key`; AWS prefers `bucket.endpoint/key`.
    path_style: bool,
}

impl S3Config {
    /// Photos are optional: without these variables the rest of the API works and
    /// photo requests answer 503.
    pub fn from_env() -> Result<Option<Self>, String> {
        let get = |key: &str| env::var(key).ok().filter(|value| !value.trim().is_empty());
        let Some(bucket) = get("S3_BUCKET") else {
            return Ok(None);
        };
        let missing = |key: &str| format!("S3_BUCKET is set but {key} is missing");
        let endpoint = get("S3_ENDPOINT").ok_or_else(|| missing("S3_ENDPOINT"))?;
        let (scheme, rest) = endpoint
            .split_once("://")
            .ok_or_else(|| "S3_ENDPOINT must start with https:// or http://".to_owned())?;
        Ok(Some(Self {
            scheme: scheme.to_owned(),
            endpoint_host: rest.trim_end_matches('/').to_owned(),
            region: get("S3_REGION").unwrap_or_else(|| "us-east-1".to_owned()),
            bucket,
            access_key_id: get("S3_ACCESS_KEY_ID").ok_or_else(|| missing("S3_ACCESS_KEY_ID"))?,
            secret_access_key: get("S3_SECRET_ACCESS_KEY")
                .ok_or_else(|| missing("S3_SECRET_ACCESS_KEY"))?,
            path_style: get("S3_FORCE_PATH_STYLE")
                .is_some_and(|value| value.eq_ignore_ascii_case("true")),
        }))
    }

    fn host_and_path(&self, key: &str) -> (String, String) {
        let key = encode_path(key);
        if self.path_style {
            (
                self.endpoint_host.clone(),
                format!("/{}/{key}", encode_path(&self.bucket)),
            )
        } else {
            (
                format!("{}.{}", self.bucket, self.endpoint_host),
                format!("/{key}"),
            )
        }
    }

    /// A URL that performs `method` on `key` until it expires. Signed headers
    /// (such as `content-type` for uploads) must be sent exactly as signed.
    pub fn presign(
        &self,
        method: &str,
        key: &str,
        expires: u64,
        headers: &[(&str, &str)],
        now: DateTime<Utc>,
    ) -> String {
        let (host, path) = self.host_and_path(key);
        let query = presigned_query(
            method,
            &host,
            &path,
            &self.region,
            &self.access_key_id,
            &self.secret_access_key,
            expires,
            headers,
            now,
        );
        format!("{}://{host}{path}?{query}", self.scheme)
    }

    /// Best effort: an object left behind only costs storage.
    pub async fn delete(&self, keys: Vec<String>) {
        if keys.is_empty() {
            return;
        }
        let Ok(client) = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
        else {
            return;
        };
        for key in keys {
            let url = self.presign("DELETE", &key, 60, &[], Utc::now());
            match client.delete(url).send().await {
                Ok(response) if response.status().is_success() => {}
                Ok(response) => eprintln!("photo delete {key}: HTTP {}", response.status()),
                Err(error) => eprintln!("photo delete {key}: {}", error.without_url()),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn presigned_query(
    method: &str,
    host: &str,
    path: &str,
    region: &str,
    access_key_id: &str,
    secret_access_key: &str,
    expires: u64,
    headers: &[(&str, &str)],
    now: DateTime<Utc>,
) -> String {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let scope = format!("{date}/{region}/s3/aws4_request");

    let mut signed: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .chain([("host".to_owned(), host.to_owned())])
        .collect();
    signed.sort();
    let signed_headers = signed
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_headers: String = signed
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();

    let mut params = vec![
        ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_owned()),
        ("X-Amz-Credential", format!("{access_key_id}/{scope}")),
        ("X-Amz-Date", amz_date.clone()),
        ("X-Amz-Expires", expires.to_string()),
        ("X-Amz-SignedHeaders", signed_headers.clone()),
    ];
    params.sort();
    let query = params
        .iter()
        .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
        .collect::<Vec<_>>()
        .join("&");

    let canonical_request = format!(
        "{method}\n{path}\n{query}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD"
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let mut key = hmac(
        format!("AWS4{secret_access_key}").as_bytes(),
        date.as_bytes(),
    );
    for part in [region, "s3", "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    let signature = hex::encode(hmac(&key, string_to_sign.as_bytes()));
    format!("{query}&X-Amz-Signature={signature}")
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// RFC 3986 encoding as SigV4 requires: only unreserved characters stay as is.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn encode_path(value: &str) -> String {
    value.split('/').map(encode).collect::<Vec<_>>().join("/")
}

/// Keys are generated by the API and scoped to their owner, so a saved photo
/// reference can be checked without asking S3.
pub fn photo_key(user_id: &str, extension: &str) -> String {
    format!(
        "photos/{user_id}/{}.{extension}",
        uuid::Uuid::new_v4().simple()
    )
}

pub fn photo_owner(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("photos/")?;
    let (owner, file) = rest.split_once('/')?;
    let (name, extension) = file.split_once('.')?;
    let valid = owner.len() == 24
        && owner.bytes().all(|byte| byte.is_ascii_hexdigit())
        && name.len() == 32
        && name.bytes().all(|byte| byte.is_ascii_hexdigit())
        && PHOTO_CONTENT_TYPES.iter().any(|(_, ext)| *ext == extension);
    valid.then_some(owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_aws_presigned_url_example() {
        // https://docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html
        let now = DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let query = presigned_query(
            "GET",
            "examplebucket.s3.amazonaws.com",
            "/test.txt",
            "us-east-1",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            86400,
            &[],
            now,
        );
        assert!(query.ends_with(
            "X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        ));
        assert!(query.contains(
            "X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request"
        ));
    }

    #[test]
    fn photo_keys_name_their_owner() {
        let owner = "0123456789abcdef01234567";
        let key = photo_key(owner, "jpg");
        assert_eq!(photo_owner(&key), Some(owner));
        assert_eq!(photo_owner("photos/../x.jpg"), None);
        assert_eq!(photo_owner(&format!("photos/{owner}/abc.exe")), None);
    }
}
