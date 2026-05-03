//! URI クエリ文字列の percent decode と key=value 取り出し。
//!
//! shpx の RDB driver はサードパーティ URL crate を持ち込まずに、`pg://` / `mssql://`
//! の `?key=value` を最低限自前で処理する方針を取っている（余計な依存を増やさないため）。
//! ここに集約することで、各 driver で `percent_decode` / `hex_val` を重複して書かない。

/// `application/x-www-form-urlencoded` 風の percent decode を行う。
///
/// `+` を空白に変換し、`%XX` を 1 バイトに復元する。不正な hex は `%` をそのまま残す。
/// マルチバイト文字はバイト列単位で復元してから `from_utf8_lossy` する（不正な UTF-8 は
/// `U+FFFD` 置換になる）。
#[must_use]
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    out.push((h << 4) | l);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// URI クエリ文字列（`?` 以降）を `(key_lowercased, value_decoded)` のイテレータに変換する。
///
/// key は ASCII lowercase 化する（DB の URL 慣習に合わせ大文字小文字非区別）。
/// `=` を含まない pair は無視する。
pub fn query_pairs(query: &str) -> impl Iterator<Item = (String, String)> + '_ {
    query.split('&').filter_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        Some((k.to_ascii_lowercase(), percent_decode(v)))
    })
}

/// URI 文字列全体から `?key=value` の値を 1 つ取り出す（percent decode 済み）。
///
/// `?` が無ければ `None`。同じキーが複数あれば最初のものを返す。`key` は小文字で渡す。
#[must_use]
pub fn query_get(raw: &str, key: &str) -> Option<String> {
    let (_, query) = raw.split_once('?')?;
    query_pairs(query).find(|(k, _)| k == key).map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("hello"), "hello");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("Sh%21px"), "Sh!px");
    }

    #[test]
    fn percent_decode_invalid_hex_kept_as_is() {
        // `%ZZ` は不正なので `%` をそのまま残し、`ZZ` も後続で処理される。
        assert_eq!(percent_decode("a%ZZb"), "a%ZZb");
    }

    #[test]
    fn percent_decode_truncated_at_end() {
        // 末尾の `%X` は 2 文字に届かないため `%` をそのまま残す。
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[test]
    fn query_pairs_lowercases_key_and_decodes_value() {
        let v: Vec<_> = query_pairs("Table=public.foo&Geom_Type=geography").collect();
        assert_eq!(
            v,
            vec![
                ("table".to_string(), "public.foo".to_string()),
                ("geom_type".to_string(), "geography".to_string()),
            ]
        );
    }

    #[test]
    fn query_pairs_ignores_pairs_without_equals() {
        let v: Vec<_> = query_pairs("a=1&bare&b=2").collect();
        assert_eq!(
            v,
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ]
        );
    }

    #[test]
    fn query_get_extracts_table() {
        assert_eq!(
            query_get("pg://h/db?table=public.cities", "table").as_deref(),
            Some("public.cities")
        );
    }

    #[test]
    fn query_get_returns_none_without_query() {
        assert!(query_get("pg://h/db", "table").is_none());
    }

    #[test]
    fn query_get_picks_first_when_duplicate() {
        assert_eq!(
            query_get("pg://h/db?table=a&table=b", "table").as_deref(),
            Some("a")
        );
    }
}
