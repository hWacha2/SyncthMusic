//! Протокол минимального Android-агента.
//!
//! ПК инициирует соединение и принимает все решения. Агент только подтверждает
//! сопряжение, отдаёт выбранную папку и выполняет файловые команды.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

use crate::transport::{self, Transport};

/// Версия протокола агента. Меняется при несовместимых правках сообщений.
///
/// Версия 2: ПК держит постоянную сессию и шлёт `GetRoot` как heartbeat,
/// иначе агент закрывает сессию после 30 секунд тишины.
pub const AGENT_PROTO: u32 = 2;

/// Сколько агент ждёт подтверждения сопряжения на телефоне.
pub const PAIR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Сколько агент ждёт хоть одного сообщения от ПК, прежде чем считать,
/// что связь потеряна. ПК шлёт heartbeat каждые несколько секунд.
pub const AGENT_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Порт, который слушает Android-агент.
pub const AGENT_PORT: u16 = 47_890;

/// Роль устройства в discovery-ответе.
pub const AGENT_ROLE: &str = "agent";

/// Запускает ответы агента на broadcast-запросы ПК.
pub fn start_discovery(dev: String, name: String, port: u16) -> Result<()> {
    crate::net::start_discovery_with(dev, name, port, AGENT_ROLE.into())
}

/// Ищет только Android-агентов, игнорируя старые демоны синхронизации.
pub fn discover(timeout: std::time::Duration) -> Result<Vec<crate::net::Discovered>> {
    let mut found = crate::net::discover_all(timeout)?;
    found.retain(|device| device.role == AGENT_ROLE);
    Ok(found)
}

/// Идентичность устройства в рукопожатии.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub dev: String,
    pub name: String,
    /// Публичный ключ Ed25519 в hex. Приватный ключ не покидает устройство.
    pub pk: String,
}

/// Один файл в манифесте удалённой папки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Путь относительно корня, всегда через `/`.
    pub path: String,
    pub size: u64,
    /// Время изменения в миллисекундах Unix. Нужно для быстрого сравнения.
    pub mtime_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum AgentMsg {
    /// ПК представился и, если уже сопряжён, прислал токен.
    Hello {
        dev: String,
        name: String,
        pk: String,
        proto: u32,
        #[serde(default)]
        token: Option<String>,
    },
    /// Агент принял рукопожатие. `paired = false` означает, что нужно спросить
    /// пользователя на телефоне.
    HelloAck {
        dev: String,
        name: String,
        pk: String,
        proto: u32,
        paired: bool,
        #[serde(default)]
        root: Option<String>,
    },
    /// ПК просит сопряжение. Агент показывает запрос пользователю.
    PairAsk {
        dev: String,
        name: String,
        pk: String,
    },
    /// Пользователь на телефоне разрешил сопряжение.
    PairOk { token: String },
    /// Пользователь на телефоне отклонил сопряжение.
    PairNo { reason: String },
    /// Запрос текущей удалённой папки.
    GetRoot,
    RootInfo {
        #[serde(default)]
        path: Option<String>,
    },
    /// ПК просит показать на телефоне выбор папки заново.
    RequestRootChange,
    /// Пользователь на телефоне выбрал новую папку.
    RootChanged { path: String },
    /// Запрос списка файлов удалённой папки.
    Scan,
    Manifest {
        root: String,
        entries: Vec<ManifestEntry>,
    },
    /// Готовность приёмника полной синхронизации на телефоне.
    SyncInfo {
        port: u16,
        #[serde(default)]
        error: Option<String>,
    },
    /// ПК спрашивает, поднят ли на телефоне приём полной синхронизации.
    GetSyncInfo,
    Error {
        message: String,
    },
    Bye,
}

/// Отправляет сообщение агенту.
pub fn send(transport: &mut dyn Transport, message: &AgentMsg) -> Result<()> {
    transport::send_json(transport, message)
}

/// Читает сообщение от агента.
pub fn recv(transport: &mut dyn Transport) -> Result<AgentMsg> {
    transport::recv_json(transport)
}

/// Собирает манифест всех обычных файлов под `root`.
///
/// Символические ссылки и каталоги пропускаются: агент работает только с
/// реальными файлами, чтобы не выйти за пределы выбранной папки.
pub fn manifest(root: &Path) -> Result<Vec<ManifestEntry>> {
    let mut entries = Vec::new();
    for item in WalkDir::new(root).follow_links(false) {
        let item = item?;
        if !item.file_type().is_file() {
            continue;
        }
        let rel = item
            .path()
            .strip_prefix(root)
            .unwrap_or(item.path())
            .to_string_lossy()
            .replace('\\', "/");
        let meta = item.metadata()?;
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|delta| delta.as_millis() as u64)
            .unwrap_or(0);
        entries.push(ManifestEntry {
            path: rel,
            size: meta.len(),
            mtime_ms,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// Проверяет, что путь существует и является каталогом.
pub fn ensure_root(path: &str) -> Result<PathBuf> {
    let root = PathBuf::from(path.trim());
    anyhow::ensure!(!path.trim().is_empty(), "путь к папке пустой");
    anyhow::ensure!(root.is_dir(), "папка не найдена: {}", root.display());
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn manifest_lists_files_relative_to_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Album")).unwrap();
        fs::write(dir.path().join("cover.jpg"), b"cover").unwrap();
        fs::write(dir.path().join("Album").join("01.flac"), b"track").unwrap();

        let entries = manifest(dir.path()).unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["Album/01.flac", "cover.jpg"]);
        assert_eq!(entries[0].size, 5);
        assert_eq!(entries[1].size, 5);
    }

    #[test]
    fn message_roundtrip_keeps_root() {
        let msg = AgentMsg::HelloAck {
            dev: "dev-pc".into(),
            name: "PC".into(),
            pk: "ab".into(),
            proto: AGENT_PROTO,
            paired: true,
            root: Some("/storage/emulated/0/Music".into()),
        };
        let text = serde_json::to_string(&msg).unwrap();
        let back: AgentMsg = serde_json::from_str(&text).unwrap();
        assert_eq!(back, msg);
    }
}
