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

/// Имя для второй версии файла: `01.sync-conflict-20261031-183000-dev-ab12.flac`.
///
/// Аудио никогда не перезаписывается: проигравшая версия остаётся рядом под
/// предсказуемым именем, которое видно в AIMP/Poweramp.
pub fn conflict_name(rel: &str, dev: &str, ms: u64) -> String {
    let (dir, file) = match rel.rsplit_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, rel),
    };
    let (stem, ext) = match file.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, Some(e)),
        _ => (file, None),
    };
    let stamp = utc_stamp(ms);
    let dev = sanitize_dev(dev);
    let mut name = format!("{stem}.sync-conflict-{stamp}-{dev}");
    if let Some(e) = ext {
        name.push('.');
        name.push_str(e);
    }
    match dir {
        Some(d) => format!("{d}/{name}"),
        None => name,
    }
}

fn sanitize_dev(dev: &str) -> String {
    let mut out: String = dev
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    out.truncate(12);
    if out.is_empty() {
        out.push_str("dev");
    }
    out
}

/// `YYYYMMDD-HHMMSS` в UTC без внешних зависимостей.
fn utc_stamp(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
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

    #[test]
    fn conflict_names_keep_extension_and_folder() {
        let name = conflict_name("Rock/Album/01.flac", "dev-ab12cd", 1_798_000_000_000);
        assert!(name.starts_with("Rock/Album/01.sync-conflict-"));
        assert!(name.ends_with("-dev-ab12cd.flac"), "{name}");
        assert_eq!(name.matches(".flac").count(), 1);

        let no_ext = conflict_name("cover", "phone", 1_798_000_000_000);
        assert!(no_ext.starts_with("cover.sync-conflict-"));
        assert!(!no_ext.ends_with(".jpg"));
    }
}
