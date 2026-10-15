use std::path::Path;

pub const UPDATE_ENV: &str = "SUTEGI_CONTRACT_UPDATE";

pub fn check_file(path: impl AsRef<Path>, rendered: &str) -> Result<(), String> {
    let path = path.as_ref();
    let update = std::env::var_os(UPDATE_ENV).is_some_and(|v| v != "0" && !v.is_empty());
    let current = match std::fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    if current.as_deref() == Some(rendered) {
        return Ok(());
    }
    if update {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
        return std::fs::write(path, rendered)
            .map_err(|e| format!("writing {}: {e}", path.display()));
    }
    let detail = match &current {
        None => "it does not exist".to_string(),
        Some(old) => first_difference(old, rendered),
    };
    Err(format!(
        "{} is out of date with the app's contract ({detail}); rerun with {UPDATE_ENV}=1 and commit the result",
        path.display()
    ))
}

fn first_difference(old: &str, new: &str) -> String {
    let mut a = old.lines();
    let mut b = new.lines();
    let mut line = 1usize;
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) if x == y => line += 1,
            (None, None) => return "line endings differ".to_string(),
            (x, y) => {
                return format!(
                    "first difference at line {line}: file has {:?}, contract renders {:?}",
                    x.unwrap_or("<end of file>"),
                    y.unwrap_or("<end of file>")
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_the_first_differing_line() {
        let d = first_difference("a\nb\nc\n", "a\nx\nc\n");
        assert!(d.contains("line 2"), "{d}");
        assert!(d.contains("\"b\""), "{d}");
    }

    #[test]
    fn matching_file_passes_and_stale_file_fails() {
        let dir = std::env::temp_dir().join(format!("sutegi-contract-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("api.ts");
        std::fs::write(&p, "one\n").unwrap();
        assert!(check_file(&p, "one\n").is_ok());
        let err = check_file(&p, "two\n").unwrap_err();
        assert!(err.contains(UPDATE_ENV), "{err}");
        let missing = check_file(dir.join("nope.ts"), "x").unwrap_err();
        assert!(missing.contains("does not exist"), "{missing}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
