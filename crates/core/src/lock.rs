//! Блокировка операций с одной библиотекой.
//!
//! Скан пишет журнал и `entries` несколькими транзакциями, а план и commit
//! читают те же таблицы через отдельное соединение SQLite. Без общей
//! блокировки план может быть построен между транзакциями скана: например,
//! увидеть уже применённый перенос в `entries`, но ещё не увидеть его
//! `entry_move` в `ops`. Тогда новый путь классифицируется как добавление,
//! commit отправляет `entry_put`, watermark пира уходит вперёд — и перенос
//! теряется навсегда.
//!
//! Блокировка привязана к каталогу данных, а не к процессу: несколько
//! соединений к одной библиотеке (UI, агентский скан, входящий сеанс) делят
//! один мьютекс, а клиент и сервер на разных библиотеках не мешают друг другу.
//! Для согласованности между разными процессами план дополнительно читается
//! одним SQLite-снимком (`Replica::read_snapshot`).
//!
//! Блокировка не реентерабельна: внутри `scan`, построения плана и commit
//! нельзя вызывать друг друга. Сейчас эти операции независимы.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

/// Мьютекс одной библиотеки. Клонируется вместе с `Arc`, поэтому владелец
/// может жить дольше конкретного `Replica`.
#[derive(Clone, Debug)]
pub struct LibraryLock(Arc<Mutex<()>>);

impl LibraryLock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(())))
    }

    /// Дождаться полного завершения текущей операции.
    ///
    /// Отравленный мьютекс не считаем фатальным: паника в одном скане не
    /// должна навсегда останавливать синхронизацию. Состояние всё равно
    /// проверяется обычными средствами (watermarks, `validate`, повторный скан).
    pub fn lock(&self) -> MutexGuard<'_, ()> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Попробовать взять блокировку без ожидания. Нужна диагностике и тестам.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, ()>> {
        match self.0.try_lock() {
            Ok(guard) => Some(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Weak<Mutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Блокировка для каталога данных. Одинаковые пути дают один и тот же мьютекс.
pub fn for_data_dir(data_dir: &Path) -> LibraryLock {
    let key = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    let mut locks = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = locks.get(&key).and_then(Weak::upgrade) {
        return LibraryLock(existing);
    }
    // Заодно вычищаем записи, чьи библиотеки уже закрыты.
    locks.retain(|_, weak| weak.strong_count() > 0);
    let fresh = LibraryLock::new();
    locks.insert(key, Arc::downgrade(&fresh.0));
    fresh
}

/// Блокировка для реплики без каталога (тесты, `in_memory`).
pub fn ephemeral() -> LibraryLock {
    LibraryLock::new()
}
