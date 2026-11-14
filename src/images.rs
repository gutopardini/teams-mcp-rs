//! Inline images from Teams messages.
//!
//! Native bytes are downloaded only from known Teams media hosts. Graph bytes
//! are downloaded only from a hosted-content URL this process builds for the
//! message. Other links are ignored.

use reqwest::Response;
use serde_json::Value;
use url::Url;

use crate::{
    auth::bounded_body,
    error::AppError,
};

pub const MAX_ATTACHED_IMAGES: usize = 4;
pub const MAX_IMAGE_BYTES: usize = 1_572_864;
const MAX_IMAGE_REFS: usize = 8;

const IMAGE_VIEWS: &[&str] = &[
    "imgo",
    "imgpsh",
    "imgpsh_fullsize",
    "imgt1",
    "imgpsh_mobile_save",
    "imgpsh_mobile_save_anim",
];

#[derive(Default)]
pub struct ImageBudget {
    attached: usize,
    attempts: usize,
}

impl ImageBudget {
    pub fn allow(&self) -> bool {
        self.attached < MAX_ATTACHED_IMAGES && self.attempts < MAX_IMAGE_REFS
    }

    pub fn note_attempt(&mut self) {
        self.attempts += 1;
    }

    pub fn note_attached(&mut self) {
        self.attached += 1;
    }
}

pub fn native_image_urls(
    html: &str,
    properties: Option<&Value>,
    mut accept: impl FnMut(&str) -> Option<String>,
) -> Vec<String> {
    let mut urls = Vec::new();
    for src in html_image_srcs(html) {
        push_unique(&mut urls, accept(&src));
        if urls.len() == MAX_IMAGE_REFS {
            return urls;
        }
    }
    if let Some(properties) = properties {
        for src in file_preview_urls(properties) {
            push_unique(&mut urls, accept(&src));
            if urls.len() == MAX_IMAGE_REFS {
                break;
            }
        }
    }
    urls
}

pub fn hosted_content_ids(html: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for src in html_image_srcs(html) {
        if let Some(id) = hosted_content_id(&src)
            && !ids.iter().any(|known| known == &id)
        {
            ids.push(id);
        }
        if ids.len() == MAX_IMAGE_REFS {
            break;
        }
    }
    ids
}

pub fn inline_ams_url(value: &str) -> Option<String> {
    let (host, id) = ams_object(value)?;
    Some(format!("https://{host}/v1/objects/{id}/views/imgo"))
}

pub fn known_view(view: &str) -> bool {
    IMAGE_VIEWS.contains(&view)
}

pub fn valid_object_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id != "."
        && id != ".."
        && !id.contains("..")
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

pub fn valid_hosted_id(id: &str) -> bool {
    (1..=200).contains(&id.len())
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '='))
}

pub fn classify_image(bytes: &[u8]) -> Result<&'static str, &'static str> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("too_large");
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Ok("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Ok("image/webp");
    }
    Err("unsupported_type")
}

pub async fn read_image_response(response: Response) -> Result<(String, Vec<u8>), &'static str> {
    let status = response.status();
    if status.is_redirection() || !status.is_success() {
        return Err("unavailable");
    }
    let bytes = match bounded_body(response, MAX_IMAGE_BYTES).await {
        Ok(bytes) => bytes,
        Err(AppError::ResponseTooLarge) => return Err("too_large"),
        Err(_) => return Err("unavailable"),
    };
    let mime = classify_image(&bytes)?;
    Ok((mime.to_owned(), bytes))
}

fn push_unique(urls: &mut Vec<String>, url: Option<String>) {
    if let Some(url) = url
        && !urls.iter().any(|known| known == &url)
    {
        urls.push(url);
    }
}

fn html_image_srcs(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut rest = 0;
    while found.len() < MAX_IMAGE_REFS {
        let Some(rel) = lower[rest..].find("<img") else {
            break;
        };
        let start = rest + rel;
        let boundary = html[start + 4..].chars().next();
        if !boundary.is_some_and(|ch| ch.is_whitespace() || ch == '/' || ch == '>') {
            rest = start + 4;
            continue;
        }
        let Some(tag) = tag_slice(&html[start..]) else {
            break;
        };
        rest = start + tag.len();
        if let Some(src) = attribute(tag, "src")
            && !src.is_empty()
        {
            found.push(src);
        }
    }
    found
}

fn tag_slice(input: &str) -> Option<&str> {
    let mut quote = None;
    for (index, ch) in input.char_indices() {
        match quote {
            Some(opened) if ch == opened => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '>' => return Some(&input[..=index]),
            None => {}
        }
    }
    None
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{name}=");
    let mut rest = 0;
    while let Some(rel) = lower[rest..].find(&needle) {
        let start = rest + rel;
        let boundary = start == 0
            || tag[..start]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_whitespace() || ch == '<');
        if !boundary {
            rest = start + needle.len();
            continue;
        }
        let value_at = start + needle.len();
        let raw = tag.get(value_at..)?;
        let quote = raw.chars().next()?;
        if quote == '"' || quote == '\'' {
            let raw = raw.get(quote.len_utf8()..)?;
            let end = raw.find(quote)?;
            return Some(decode_entities(&raw[..end]));
        }
        let end = raw
            .find(|ch: char| ch.is_whitespace() || ch == '>')
            .unwrap_or(raw.len());
        return Some(decode_entities(&raw[..end]));
    }
    None
}

fn decode_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn file_preview_urls(properties: &Value) -> Vec<String> {
    let Some(files) = properties.get("files") else {
        return Vec::new();
    };
    let list = if let Some(text) = files.as_str() {
        serde_json::from_str::<Vec<Value>>(text).unwrap_or_default()
    } else if let Some(list) = files.as_array() {
        list.clone()
    } else {
        return Vec::new();
    };
    list.iter().filter_map(file_preview_url).collect()
}

fn file_preview_url(file: &Value) -> Option<String> {
    let preview = file
        .pointer("/filePreview/previewUrl")
        .and_then(Value::as_str)?;
    if preview.is_empty() {
        return None;
    }
    let explicit = file.get("type").and_then(Value::as_str).unwrap_or("");
    let title = file.get("title").and_then(Value::as_str).unwrap_or("");
    let image = if !explicit.is_empty() {
        image_type(explicit)
    } else if !title.is_empty() {
        image_type(title)
    } else {
        true
    };
    image.then(|| preview.to_owned())
}

fn image_type(value: &str) -> bool {
    let kind = value
        .rsplit(['.', '/'])
        .next()
        .unwrap_or(value)
        .to_ascii_lowercase();
    matches!(
        kind.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "image" | "photo"
    )
}

fn hosted_content_id(src: &str) -> Option<String> {
    let lower = src.to_ascii_lowercase();
    let marker = "hostedcontents/";
    let start = lower.find(marker)? + marker.len();
    let rest = src.get(start..)?;
    let end = rest.find('/').unwrap_or(rest.len());
    let id = &rest[..end];
    if !valid_hosted_id(id) {
        return None;
    }
    let after = lower.get(start + end..)?;
    (after.starts_with("/$value") || after.starts_with("/%24value")).then(|| id.to_owned())
}

fn ams_object(value: &str) -> Option<(String, String)> {
    if value.len() > 2048 {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return None;
    }
    let host = url.host_str()?.to_owned();
    if !ams_host(&host) {
        return None;
    }
    let segments = url.path_segments()?.collect::<Vec<_>>();
    if segments.len() != 5
        || segments[0] != "v1"
        || segments[1] != "objects"
        || segments[3] != "views"
        || !known_view(segments[4])
        || !valid_object_id(segments[2])
    {
        return None;
    }
    Some((host, segments[2].to_owned()))
}

fn ams_host(host: &str) -> bool {
    if host.starts_with('.') || host.ends_with('.') || host.contains("..") {
        return false;
    }
    host == "api.asm.skype.com"
        || host.ends_with(".asm.skype.com")
        || host.ends_with(".asyncgw.teams.microsoft.com")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ams_urls_are_rewritten_and_other_hosts_are_rejected() {
        assert_eq!(
            inline_ams_url(
                "https://us-api.asm.skype.com/v1/objects/0-abc/views/imgpsh_fullsize?x=1"
            )
            .as_deref(),
            Some("https://us-api.asm.skype.com/v1/objects/0-abc/views/imgo")
        );
        assert_eq!(
            inline_ams_url("https://media.asyncgw.teams.microsoft.com/v1/objects/0-abc/views/imgt1")
                .as_deref(),
            Some("https://media.asyncgw.teams.microsoft.com/v1/objects/0-abc/views/imgo")
        );
        for url in [
            "http://api.asm.skype.com/v1/objects/0-abc/views/imgo",
            "https://user@api.asm.skype.com/v1/objects/0-abc/views/imgo",
            "https://api.asm.skype.com.evil.test/v1/objects/0-abc/views/imgo",
            "https://evil.asm.skype.com.attacker/v1/objects/0-abc/views/imgo",
            "https://api.asm.skype.com/v1/objects/../views/imgo",
            "https://api.asm.skype.com/v1/objects/0-abc/content/imgpsh",
        ] {
            assert!(inline_ams_url(url).is_none(), "{url}");
        }
    }

    #[test]
    fn html_and_file_previews_keep_image_urls_only() {
        let html = "<p>see</p><img src='https://us-api.asm.skype.com/v1/objects/0-a/views/imgo' itemtype='http://schema.skype.com/AMSImage'><img src=\"https://evil.test/x.png\"><img src='https://us-api.asm.skype.com/v1/objects/0-a/views/imgpsh'>";
        let properties = json!({
            "files": json!([
                {"id":"png","type":"png","title":"capture.png","filePreview":{"previewUrl":"https://us-api.asm.skype.com/v1/objects/0-b/views/imgpsh_fullsize"}},
                {"id":"pdf","type":"pdf","title":"notes.pdf","filePreview":{"previewUrl":"https://us-api.asm.skype.com/v1/objects/0-c/views/imgo"}}
            ]).to_string()
        });
        let urls = native_image_urls(html, Some(&properties), inline_ams_url);
        assert_eq!(
            urls,
            vec![
                "https://us-api.asm.skype.com/v1/objects/0-a/views/imgo",
                "https://us-api.asm.skype.com/v1/objects/0-b/views/imgo",
            ]
        );
    }

    #[test]
    fn hosted_content_ids_come_from_img_sources() {
        let html = "<p>hostedContents/ignore/$value</p><img src='https://evil.example/hostedContents/abc/$value'><img src=\"../hostedContents/abc/%24value\"><img src='../hostedContents/second/$value'>";
        assert_eq!(hosted_content_ids(html), vec!["abc".to_owned(), "second".to_owned()]);
    }

    #[test]
    fn image_bytes_are_classified_by_signature() {
        assert_eq!(classify_image(b"\x89PNG\r\n\x1a\nrest"), Ok("image/png"));
        assert_eq!(classify_image(b"\xff\xd8\xffrest"), Ok("image/jpeg"));
        assert_eq!(classify_image(b"GIF89a"), Ok("image/gif"));
        assert_eq!(classify_image(b"RIFFxxxxWEBP"), Ok("image/webp"));
        assert_eq!(classify_image(b"<html>"), Err("unsupported_type"));
        let mut huge = vec![0x89, b'P', b'N', b'G'];
        huge.resize(MAX_IMAGE_BYTES + 1, 0);
        assert_eq!(classify_image(&huge), Err("too_large"));
    }
}
