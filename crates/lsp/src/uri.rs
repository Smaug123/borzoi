//! File-path and string conversions for [`lsp_types::Uri`].
//!
//! `lsp_types::Uri` wraps a strict RFC 3986 parser and offers no file-path
//! conversion, so [`UriExt`] supplies one. Every conversion goes through the
//! `url` crate (WHATWG URL semantics): a path maps to the URI `url` would give
//! it, and a URI maps back to the path `url` would decode from it.
//!
//! WHATWG and RFC 3986 disagree about which characters may appear unescaped.
//! `url` leaves `[`, `]`, `^`, `|`, `` ` ``, `{`, `}` (and, in a query or
//! fragment, a second `#`; anywhere, a malformed `%` escape) in a path, query
//! or fragment, where RFC 3986 requires them percent-encoded. `rfc3986`
//! encodes exactly those, so the conversion is total: a file named `A[1].fs`
//! still has a URI. Percent-encoding a character does not change what a URI
//! denotes once decoded, so [`UriExt::to_file_path`] returns the same path for
//! the encoded spelling as `url` does for its own.

use std::fmt;
use std::path::{Path, PathBuf};

use lsp_types::Uri;

/// Why [`UriExt::parse`] refused a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UriParseError {
    /// `url` refused it as an absolute URL.
    Url(url::ParseError),
    /// `url` accepted it, but its RFC 3986 respelling is not a URI. The
    /// respelling is built to be one, so this marks a bug in `rfc3986`.
    Respelling(String),
}

impl fmt::Display for UriParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UriParseError::Url(e) => e.fmt(f),
            UriParseError::Respelling(spelled) => {
                write!(f, "RFC 3986 respelling is not a URI: {spelled}")
            }
        }
    }
}

impl std::error::Error for UriParseError {}

/// `url`-backed construction and file-path conversion for [`Uri`].
///
/// The file-path signatures are `url::Url`'s own, `Result<_, ()>` included:
/// there is one way each can fail, and callers match on it as they did there.
#[allow(clippy::result_unit_err)]
pub trait UriExt: Sized {
    /// Parse `s` as an absolute URL with `url`'s (lenient, normalising) rules.
    fn parse(s: &str) -> Result<Self, UriParseError>;
    /// The `file:` URI of an absolute path; `Err` for a relative one.
    fn from_file_path(path: impl AsRef<Path>) -> Result<Self, ()>;
    /// As [`UriExt::from_file_path`], with a trailing `/`.
    fn from_directory_path(path: impl AsRef<Path>) -> Result<Self, ()>;
    /// The path a `file:` URI denotes; `Err` for any other URI.
    fn to_file_path(&self) -> Result<PathBuf, ()>;
}

impl UriExt for Uri {
    fn parse(s: &str) -> Result<Self, UriParseError> {
        let url = url::Url::parse(s).map_err(UriParseError::Url)?;
        from_url(&url).map_err(UriParseError::Respelling)
    }

    fn from_file_path(path: impl AsRef<Path>) -> Result<Self, ()> {
        from_url(&url::Url::from_file_path(path)?).map_err(drop)
    }

    fn from_directory_path(path: impl AsRef<Path>) -> Result<Self, ()> {
        from_url(&url::Url::from_directory_path(path)?).map_err(drop)
    }

    fn to_file_path(&self) -> Result<PathBuf, ()> {
        url::Url::parse(self.as_str())
            .map_err(|_| ())?
            .to_file_path()
    }
}

/// The [`Uri`] spelling of a parsed `url::Url`, or the respelling that
/// failed to parse (see [`UriParseError::Respelling`]).
fn from_url(url: &url::Url) -> Result<Uri, String> {
    let spelled = rfc3986(url);
    spelled.parse().map_err(|_| spelled)
}

/// `url`'s serialisation re-encoded to RFC 3986, component by component.
///
/// The scheme, port and IP-literal hosts are already RFC-valid. A domain host
/// keeps `reg-name` characters, userinfo keeps those plus `:` inside the
/// password, and everything from the path on keeps `pchar`, `/` and `?`, plus
/// the first `#` (the fragment delimiter: `url` writes it unescaped and only
/// where a fragment starts). Any other byte becomes `%XX`, as does a `%` that
/// does not start a two-hex-digit escape.
fn rfc3986(url: &url::Url) -> String {
    let mut out = String::with_capacity(url.as_str().len());
    out.push_str(url.scheme());
    out.push(':');
    if url.has_authority() {
        out.push_str("//");
        if !url.username().is_empty() || url.password().is_some() {
            escape_into(&mut out, url.username(), |_| false);
            if let Some(password) = url.password() {
                out.push(':');
                escape_into(&mut out, password, |b| b == b':');
            }
            out.push('@');
        }
        match url.host() {
            Some(url::Host::Domain(domain)) => escape_into(&mut out, domain, |_| false),
            Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)) => {
                out.push_str(&url[url::Position::BeforeHost..url::Position::AfterHost]);
            }
            None => {}
        }
        if let Some(port) = url.port() {
            out.push(':');
            out.push_str(&port.to_string());
        }
    }
    let mut fragment_pending = url.fragment().is_some();
    escape_into(&mut out, &url[url::Position::BeforePath..], |b| match b {
        b':' | b'@' | b'/' | b'?' => true,
        b'#' if fragment_pending => {
            fragment_pending = false;
            true
        }
        _ => false,
    });
    out
}

/// Append `s` to `out`, keeping RFC 3986 `unreserved`, `sub-delims`, valid
/// `%XX` escapes, and any byte `extra` admits; every other byte is
/// percent-encoded.
fn escape_into(out: &mut String, s: &str, mut extra: impl FnMut(u8) -> bool) {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let keep = match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => true,
            b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' => true,
            b'%' => bytes
                .get(i + 1..i + 3)
                .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)),
            _ => extra(b),
        };
        if keep {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Lenient percent-decoding: the bytes a URI string denotes.
    fn decoded(s: &str) -> Vec<u8> {
        percent_decode(s.as_bytes())
    }

    fn percent_decode(b: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%'
                && let Some(h) = b.get(i + 1..i + 3)
                && h.iter().all(u8::is_ascii_hexdigit)
                && let Ok(s) = std::str::from_utf8(h)
                && let Ok(v) = u8::from_str_radix(s, 16)
            {
                out.push(v);
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn rfc_illegal_path_characters_are_encoded() {
        let uri = Uri::from_file_path("/src/A[1]^|`{x}.fs").unwrap();
        assert_eq!(uri.as_str(), "file:///src/A%5B1%5D%5E%7C%60%7Bx%7D.fs");
        assert_eq!(
            uri.to_file_path().unwrap(),
            PathBuf::from("/src/A[1]^|`{x}.fs")
        );
    }

    #[test]
    fn ordinary_file_uri_is_url_spelling() {
        let uri = Uri::from_file_path("/home/me/My Lib/Lib.fs").unwrap();
        assert_eq!(uri.as_str(), "file:///home/me/My%20Lib/Lib.fs");
    }

    #[test]
    fn parse_keeps_query_and_fragment_structure() {
        let uri = Uri::parse("https://h/p?q=[1]#frag#more").unwrap();
        assert_eq!(uri.as_str(), "https://h/p?q=%5B1%5D#frag%23more");
        assert_eq!(uri.query().unwrap().as_str(), "q=%5B1%5D");
        assert_eq!(uri.fragment().unwrap().as_str(), "frag%23more");
    }

    #[test]
    fn malformed_percent_escape_is_encoded() {
        let uri = Uri::parse("https://h/a%zz%4").unwrap();
        assert_eq!(uri.as_str(), "https://h/a%25zz%254");
    }

    /// `url` keeps a path's dot segments when spelling it but resolves
    /// them on every parse, so the path a URI denotes is the lexically
    /// resolved one, as it was whenever a URI round-tripped through a client.
    #[test]
    fn dot_segments_resolve_on_the_way_back() {
        let uri = Uri::from_file_path("/proj/src/../Shared/A.fs").unwrap();
        assert_eq!(uri.as_str(), "file:///proj/src/../Shared/A.fs");
        assert_eq!(
            uri.to_file_path().unwrap(),
            PathBuf::from("/proj/Shared/A.fs")
        );
    }

    #[test]
    fn relative_path_and_non_file_uri_are_refused() {
        assert!(Uri::from_file_path("rel/A.fs").is_err());
        assert!(
            Uri::parse("https://h/A.fs")
                .unwrap()
                .to_file_path()
                .is_err()
        );
    }

    proptest! {
        /// Any absolute path has a URI, and it decodes to the path `url`
        /// decodes from its own spelling once re-parsed (as it is when the
        /// URI crosses the wire), so dot segments come back resolved.
        #[test]
        fn file_path_round_trips_like_url(segments in prop::collection::vec("\\PC{0,8}", 1..5)) {
            let path: PathBuf = std::iter::once("/".to_string())
                .chain(segments.into_iter().map(|s| s.replace(['/', '\0'], "_")))
                .collect();
            let Ok(reference) = url::Url::from_file_path(&path) else {
                return Ok(());
            };
            let uri = Uri::from_file_path(&path).expect("url accepted the path");
            let reparsed = url::Url::parse(reference.as_str()).expect("url reparses its own output");
            prop_assert_eq!(uri.to_file_path(), reparsed.to_file_path());
            prop_assert_eq!(decoded(uri.as_str()), decoded(reference.as_str()));
        }

        /// Every string `url` parses becomes a URI denoting the same bytes.
        #[test]
        fn parse_is_total_over_url(s in "(https?|file|ftp|x-y):(//[a-z0-9.@:]{0,10}/)?\\PC{0,32}") {
            let Ok(reference) = url::Url::parse(&s) else {
                return Ok(());
            };
            let uri = Uri::parse(&s).expect("url accepted the string");
            prop_assert_eq!(decoded(uri.as_str()), decoded(reference.as_str()));
            let back = url::Url::parse(uri.as_str()).expect("respelling still parses");
            prop_assert_eq!(back.scheme(), reference.scheme());
            prop_assert_eq!(back.host_str(), reference.host_str());
            prop_assert_eq!(back.port(), reference.port());
            prop_assert_eq!(decoded(back.path()), decoded(reference.path()));
        }
    }
}
