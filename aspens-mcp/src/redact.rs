//! Strip URLs from text before it reaches the agent: RPC URLs can embed API keys.

/// Schemes whose URLs are replaced. `ws(s)` RPC endpoints carry keys too.
const SCHEMES: [&str; 4] = ["https://", "http://", "wss://", "ws://"];

fn ends_url(c: char) -> bool {
    c.is_whitespace() || c == '"' || c == '\''
}

/// Replace every `http(s)://` (and `ws(s)://`) URL in `s`, up to the next
/// whitespace or quote, with `<url>`. Schemes match case-insensitively.
pub fn redact(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if let Some(scheme) = SCHEMES.iter().find(|p| lower[i..].starts_with(*p)) {
            let rest = &s[i + scheme.len()..];
            let end = rest.find(ends_url).unwrap_or(rest.len());
            out.push_str("<url>");
            i += scheme.len() + end;
        } else {
            // Advance one whole char; ASCII lowering keeps byte offsets aligned.
            let c = s[i..].chars().next().unwrap_or_default();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_urls_up_to_whitespace_or_quote() {
        // Punctuation glued to the URL goes with it: over-redacting is safe.
        assert_eq!(
            redact("error sending request for url (https://rpc.example/v2/KEY): timed out"),
            "error sending request for url (<url> timed out"
        );
    }

    #[test]
    fn url_runs_to_whitespace() {
        assert_eq!(
            redact("rpc https://a.example/KEY failed"),
            "rpc <url> failed"
        );
    }

    #[test]
    fn url_stops_at_quotes() {
        assert_eq!(
            redact(r#"url "http://a.example/KEY" and 'https://b/K2'"#),
            r#"url "<url>" and '<url>'"#
        );
    }

    #[test]
    fn every_scheme_and_case() {
        assert_eq!(
            redact("HTTPS://A/K1 wss://b/K2 ws://c/K3 http://d/K4"),
            "<url> <url> <url> <url>"
        );
    }

    #[test]
    fn url_at_end_and_non_ascii_survive() {
        assert_eq!(redact("é → https://x/KEY"), "é → <url>");
        assert_eq!(redact("no url here"), "no url here");
        assert_eq!(redact("https://"), "<url>");
    }
}
