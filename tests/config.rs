//! The second half of the M2 exit test: every invalid configuration example
//! is rejected with a message that names the line the problem is on.
//!
//! Each file in `tests/config/invalid/` starts with a comment that says the
//! line and a piece of the message, `# line 7: there is no upstream group`.

use std::fs;
use std::path::Path;

use waystation::Config;

// `cfg(test)` lets clippy treat this helper as test code, where `unwrap` is
// allowed.
#[cfg(test)]
fn examples(kind: &str) -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/config")
        .join(kind);
    let mut files: Vec<(String, String)> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(&path).unwrap())
        })
        .collect();
    files.sort();
    files
}

#[test]
fn every_invalid_example_is_rejected_with_the_line_it_is_on() {
    let examples = examples("invalid");
    assert!(examples.len() >= 25, "{} examples", examples.len());
    let mut wrong = Vec::new();
    for (name, text) in &examples {
        let expected = text
            .lines()
            .next()
            .and_then(|first| first.strip_prefix("# line "))
            .and_then(|rest| rest.split_once(": "))
            .unwrap_or_else(|| panic!("{name} does not start with `# line N: message`"));
        let line: usize = expected.0.parse().unwrap();
        match Config::from_toml(text) {
            Ok(_) => wrong.push(format!("{name}: accepted")),
            Err(error) => {
                let message = error.to_string();
                if error.line() != line
                    || !message.starts_with(&format!("line {line}, column "))
                    || !error.message().contains(expected.1)
                {
                    wrong.push(format!("{name}: {message}"));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn every_valid_example_is_accepted() {
    for (name, text) in examples("valid") {
        if let Err(error) = Config::from_toml(&text) {
            panic!("{name}: {error}");
        }
    }
}

#[test]
fn the_listen_address_is_read_from_the_file() {
    let config = Config::from_toml(
        r#"
        listen = "[::1]:9443"
        [upstreams.app]
        servers = [{ address = "127.0.0.1:9001" }]
        [[routes]]
        upstream = "app"
        "#,
    )
    .unwrap();
    assert_eq!(config.listen(), "[::1]:9443".parse().unwrap());
}
