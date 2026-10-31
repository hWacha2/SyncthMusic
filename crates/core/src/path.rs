//! Нормализация относительных путей.
//!
//! Путь — часть реплицируемого состояния (ADR-002), поэтому он обязан быть
//! каноническим: одинаковые байты на всех платформах и никаких `..`.

use anyhow::{bail, Result};

/// Приводит путь к виду `a/b/c.ext` (прямые слэши, без `.` и `..`).
pub fn normalize(rel: &str) -> Result<String> {
    let s = rel.replace('\\', "/");
    if s.starts_with('/') {
        bail!("абсолютный путь недопустим: {rel}");
    }
    if s.contains(':') {
        bail!("путь с диском недопустим: {rel}");
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in s.split('/') {
        match part {
            "" | "." => continue,
            ".." => bail!("путь выходит за корень библиотеки: {rel}"),
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        bail!("пустой путь");
    }
    let joined = parts.join("/");
    if joined.len() > 4096 {
        bail!("слишком длинный путь");
    }
    Ok(joined)
}

/// Путь не должен попадать в служебные каталоги.
pub fn is_service_path(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    lower.starts_with(".syncth") || lower.contains("/.syncth")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_windows_and_unix_separators() {
        assert_eq!(
            normalize(r"Rock\Album\01.flac").unwrap(),
            "Rock/Album/01.flac"
        );
        assert_eq!(
            normalize("Rock//Album/./01.flac").unwrap(),
            "Rock/Album/01.flac"
        );
    }

    #[test]
    fn rejects_escapes_and_absolute_paths() {
        assert!(normalize("../etc/passwd").is_err());
        assert!(normalize("/etc/passwd").is_err());
        assert!(normalize(r"C:\Music\a.flac").is_err());
        assert!(normalize("").is_err());
    }

    #[test]
    fn detects_service_paths() {
        assert!(is_service_path(".syncth/ops/x.json"));
        assert!(is_service_path("a/.syncth/x"));
        assert!(!is_service_path("Rock/.hidden/x.flac"));
    }
}
