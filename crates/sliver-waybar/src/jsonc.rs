//! JSON with comments and trailing commas (what waybar's jsoncpp reads) to plain JSON.

/// Remove `//` and `/* */` comments and commas before `}` / `]`, leaving strings alone.
pub fn strip(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                out.push(c);
                i += 1;
                while i < chars.len() {
                    out.push(chars[i]);
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        out.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if chars[i] == '"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    // keep newlines so error line numbers stay right
                    if chars[i] == '\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
                i += 2;
            }
            ',' => {
                // drop it if only whitespace/comments separate it from a closer
                let mut j = i + 1;
                loop {
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'/') {
                        while j < chars.len() && chars[j] != '\n' {
                            j += 1;
                        }
                    } else if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'*') {
                        j += 2;
                        while j + 1 < chars.len() && !(chars[j] == '*' && chars[j + 1] == '/') {
                            j += 1;
                        }
                        j += 2;
                    } else {
                        break;
                    }
                }
                if !matches!(chars.get(j), Some('}') | Some(']')) {
                    out.push(',');
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

pub fn parse(src: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(&strip(src)).map_err(|e| format!("not valid JSON(C): {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_comments_and_trailing_commas() {
        let v = parse(
            r#"
            // top comment
            {
              "a": "x // not a comment", /* block */
              "b": [1, 2, /* c */ 3, ],
              "c": { "d": "q\"//", },
            }
            "#,
        )
        .unwrap();
        assert_eq!(v["a"], "x // not a comment");
        assert_eq!(v["b"], serde_json::json!([1, 2, 3]));
        assert_eq!(v["c"]["d"], "q\"//");
    }

    #[test]
    fn reports_bad_input() {
        assert!(parse("{ nope").unwrap_err().contains("not valid JSON"));
    }
}
