use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::time::Duration;
use syncth_core::{net, pipe, resolve, scan, vault, Action, Replica};

#[derive(Parser, Debug)]
#[command(
    name = "syncth",
    version,
    about = "Синхронизация музыкальной библиотеки без сервера"
)]
struct Cli {
    /// Каталог состояния (БД, ключи). Не должен лежать внутри библиотеки.
    #[arg(long, default_value = ".syncth-data", global = true)]
    data_dir: PathBuf,

    /// Идентификатор устройства (по умолчанию — из БД).
    #[arg(long, global = true)]
    dev: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Инициализировать устройство и указать корень библиотеки.
    Init {
        #[arg(long)]
        root: PathBuf,
    },
    /// Просканировать корень: найти новые, изменённые и пропавшие файлы.
    Scan,
    /// Показать состояние реплики.
    Status,
    /// Экспортировать операции в каталог-трубу.
    Export {
        #[arg(long)]
        dir: PathBuf,
    },
    /// Импортировать операции из каталога-трубы.
    Import {
        #[arg(long)]
        dir: PathBuf,
    },
    /// Проверить, что файлы на диске соответствуют хешам в БД.
    Verify,
    /// Ожидающие удаления: решения принимает человек.
    Pending {
        #[command(subcommand)]
        action: PendingCmd,
    },
    /// Не держать файл на этом устройстве (остаётся в общей библиотеке).
    Ignore { path: String },
    /// Снова держать файл здесь (докачается при обмене).
    Unignore { path: String },
    /// Показать или перевыпустить общий токен библиотеки.
    Token {
        /// Выпустить новый токен (придётся обновить на всех устройствах).
        #[arg(long)]
        rotate: bool,
    },
    /// Слушать LAN и обслуживать подключения.
    Serve {
        #[arg(long, default_value = "0.0.0.0:0")]
        bind: String,
        #[arg(long)]
        token: Option<String>,
    },
    /// Синхронизироваться с устройством по адресу.
    Sync {
        #[arg(long)]
        addr: String,
        #[arg(long)]
        token: Option<String>,
    },
    /// Найти устройства в локальной сети.
    Discover {
        #[arg(long, default_value_t = 2)]
        seconds: u64,
    },
    /// Корзина: удалённое можно вернуть, пока не истёк срок хранения.
    Vault {
        #[command(subcommand)]
        action: VaultCmd,
    },
}

#[derive(Subcommand, Debug)]
enum PendingCmd {
    /// Список ожидающих удалений.
    List,
    /// Решить судьбу одного удаления.
    Resolve {
        path: String,
        /// Удалить трек из библиотеки на всех устройствах (копия остаётся в корзине).
        #[arg(long, conflicts_with = "only_here")]
        delete_everywhere: bool,
        /// Убрать только с этого устройства, в общей библиотеке оставить.
        #[arg(long)]
        only_here: bool,
    },
    /// Вернуть файл на это устройство (докачается при обмене).
    Restore { path: String },
    /// Снять ожидания, ничего не удаляя (например, файлы вернули вручную).
    Forget { path: Option<String> },
}

#[derive(Subcommand, Debug)]
enum VaultCmd {
    /// Что лежит в корзине.
    List,
    /// Вернуть трек из корзины в библиотеку (уедет на другие устройства).
    Restore { path: String },
    /// Удалить из корзины всё старше срока и файлы-сироты.
    Gc {
        #[arg(long, default_value_t = vault::DEFAULT_TTL_DAYS)]
        days: u64,
    },
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    if let Err(e) = run() {
        eprintln!("ошибка: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let dev = cli
        .dev
        .clone()
        .or_else(|| std::env::var("SYNCTH_DEV").ok())
        .unwrap_or_else(|| default_device_id(&cli.data_dir));

    let mut replica = Replica::open(&cli.data_dir, &dev)?;
    let dev = replica.device_id().to_string();

    match cli.command {
        Command::Init { root } => {
            let root = absolute(&root)?;
            if !root.is_dir() {
                bail!("каталог не существует: {}", root.display());
            }
            if is_inside(&cli.data_dir, &root) {
                bail!("data-dir не должен находиться внутри библиотеки");
            }
            replica.set_root(&root)?;
            replica.set_meta("device_id", &dev)?;
            let token = replica.ensure_token()?;
            println!("инициализировано");
            println!("  устройство: {}", dev);
            println!("  корень:     {}", root.display());
            println!("  данные:     {}", absolute(&cli.data_dir)?.display());
            println!("  токен:      {token}");
            println!();
            println!("Тот же токен нужен второму устройству: syncth token --rotate");
        }

        Command::Scan => {
            let root = replica
                .root()?
                .context("сначала выполните: syncth init --root <путь>")?;
            let report = scan::scan(&root, &mut replica)?;
            println!("скан завершён");
            println!("  новых:      {}", report.added);
            println!("  изменённых: {}", report.changed);
            println!("  без изм.:   {}", report.unchanged);
            println!("  занятых:    {}", report.skipped_busy);
            println!("  операций:   {}", report.ops_emitted);
            if !report.missing.is_empty() {
                println!();
                println!("пропало с диска: {} файл(ов)", report.missing.len());
                for p in report.missing.iter().take(20) {
                    println!("  - {p}");
                }
                if report.missing.len() > 20 {
                    println!("  ... и ещё {}", report.missing.len() - 20);
                }
                println!();
                println!("Удаление НЕ применено. Решение — за вами:");
                println!("  syncth pending list");
                println!("  syncth pending resolve \"<путь>\" --delete-everywhere | --only-here");
            }
        }

        Command::Status => {
            println!("устройство: {}", replica.device_id());
            match replica.root()? {
                Some(r) => println!("корень:     {}", r.display()),
                None => println!("корень:     не задан"),
            }
            println!("операций:   {}", replica.op_count()?);
            println!("файлов:     {}", replica.live_entries()?.len());
            println!("здесь:      {}", replica.local_entries()?.len());
            println!("размер:     {}", human(replica.total_live_bytes()?));
            println!("state:      {}", replica.state_hash()?);
            let pending = replica.pending_deletes()?;
            if pending.is_empty() {
                println!("удаления:   нет ожидающих");
            } else {
                println!("удаления:   {} ожидает решения", pending.len());
                for p in pending.iter().take(10) {
                    println!("  - {}", p.path);
                }
            }
            let ignored = replica.ignored_paths()?;
            if !ignored.is_empty() {
                println!(
                    "исключено:  {} (есть в библиотеке, нет здесь)",
                    ignored.len()
                );
            }
            println!("корзина:    {}", replica.vault_rows()?.len());
        }

        Command::Export { dir } => {
            let n = pipe::export_ops(&replica, &dir)?;
            println!("экспортировано операций: {n}");
        }

        Command::Import { dir } => {
            let n = pipe::import_ops(&mut replica, &dir)?;
            println!("применено операций: {n}");
            if let Some(root) = replica.root()? {
                let saved = resolve::prune_deleted_files(&mut replica, &root)?;
                if !saved.is_empty() {
                    println!("сохранено конфликтных копий: {}", saved.len());
                    for p in saved {
                        println!("  + {p}");
                    }
                }
            }
            println!("state: {}", replica.state_hash()?);
        }

        Command::Verify => {
            let root = replica.root()?.context("корень не задан")?;
            let mut ok = 0usize;
            let mut bad = 0usize;
            let mut missing = 0usize;
            for e in replica.live_entries()? {
                if replica.is_ignored(&e.path)? {
                    continue;
                }
                let abs = resolve::disk_path(&root, &e.path);
                if !abs.is_file() {
                    missing += 1;
                    println!("нет файла: {}", e.path);
                    continue;
                }
                match scan::hash_file(&abs) {
                    Ok(h) if h == e.blob => ok += 1,
                    Ok(_) => {
                        bad += 1;
                        println!("хеш не совпал: {}", e.path);
                    }
                    Err(err) => {
                        bad += 1;
                        println!("не читается {}: {err}", e.path);
                    }
                }
            }
            println!("проверено: {ok} ок, {bad} расхождений, {missing} отсутствует");
            if bad > 0 || missing > 0 {
                std::process::exit(2);
            }
        }

        Command::Pending { action } => {
            match action {
                PendingCmd::List => {
                    let pending = replica.pending_deletes()?;
                    if pending.is_empty() {
                        println!("нет ожидающих удалений");
                    } else {
                        println!("{} ожидает решения:", pending.len());
                        for p in pending {
                            let copies = replica.live_paths_by_blob(&p.blob)?;
                            println!("  {}", p.path);
                            if copies.len() > 1 {
                                println!("    тот же трек ещё здесь:");
                                for c in copies.iter().filter(|c| **c != p.path) {
                                    println!("      {c}");
                                }
                            }
                        }
                        println!();
                        println!("Решения:");
                        println!("  syncth pending resolve \"<путь>\" --delete-everywhere");
                        println!("  syncth pending resolve \"<путь>\" --only-here");
                    }
                }
                PendingCmd::Resolve {
                    path,
                    delete_everywhere,
                    only_here,
                } => {
                    let action = match (delete_everywhere, only_here) {
                        (true, false) => Action::DeleteEverywhere,
                        (false, true) => Action::OnlyHere,
                        _ => bail!("укажите --delete-everywhere или --only-here"),
                    };
                    let out = resolve::resolve(&mut replica, &path, action)?;
                    match action {
                        Action::DeleteEverywhere => {
                            println!("удалено из библиотеки: {}", out.deleted_paths.len());
                            for p in &out.deleted_paths {
                                println!("  - {p}");
                            }
                            println!("файлов удалено здесь: {}", out.files_removed);
                            println!("операций: {}", out.ops_emitted);
                            println!("Копия сохранена в корзине: syncth vault list");
                            println!("Удаление уедет на другие устройства при следующем обмене.");
                        }
                        Action::OnlyHere => {
                            println!("убрано только с этого устройства: {path}");
                            println!("В общей библиотеке файл остался, другие устройства его не потеряют.");
                            println!("Вернуть: syncth pending restore \"{path}\"");
                        }
                    }
                    if !out.failed.is_empty() {
                        println!("не удалось удалить:");
                        for (p, e) in out.failed {
                            println!("  - {p}: {e}");
                        }
                        std::process::exit(2);
                    }
                }
                PendingCmd::Restore { path } => {
                    resolve::restore(&mut replica, &path)?;
                    println!("«{path}» снова в локальной библиотеке.");
                    println!("Если файла здесь нет, он докачается при следующем обмене.");
                }
                PendingCmd::Forget { path } => match path {
                    Some(p) => {
                        replica.clear_pending_delete(&p)?;
                        println!("ожидание для {p} снято (файл не удалён)");
                    }
                    None => {
                        let n = replica.pending_deletes()?.len();
                        for p in replica.pending_deletes()? {
                            replica.clear_pending_delete(&p.path)?;
                        }
                        println!("снято ожиданий: {n} (ничего не удалено)");
                    }
                },
            }
        }

        Command::Ignore { path } => {
            ensure_entry(&replica, &path)?;
            replica.set_ignored(&path, true)?;
            replica.clear_pending_delete(&path)?;
            println!("«{path}» исключён на этом устройстве; в общей библиотеке остался.");
        }
        Command::Unignore { path } => {
            ensure_entry(&replica, &path)?;
            replica.set_ignored(&path, false)?;
            replica.mark_absent(&path)?;
            println!("«{path}» снова ожидается здесь; докачается при обмене.");
        }

        Command::Token { rotate } => {
            let token = if rotate {
                replica.rotate_token()?
            } else {
                replica.ensure_token()?
            };
            println!("{token}");
        }

        Command::Serve { bind, token } => {
            let token = match token {
                Some(t) => t,
                None => replica.ensure_token()?,
            };
            println!("токен: syncth token");
            net::serve(&mut replica, &bind, &token, |_stats| {})?;
        }

        Command::Sync { addr, token } => {
            let token = token.or_else(|| std::env::var("SYNCTH_TOKEN").ok());
            let token = match token {
                Some(t) => t,
                None => replica.ensure_token()?,
            };
            // Свежий скан перед обменом: без него телефон отправит устаревший манифест.
            if let Some(root) = replica.root()? {
                let report = scan::scan(&root, &mut replica)?;
                if report.ops_emitted > 0 {
                    println!("перед синхронизацией: +{} изменений", report.ops_emitted);
                }
            }
            let stats = net::sync_with_peer(&mut replica, &addr, &token)?;
            println!("синхронизировано с {addr}");
            println!("  {}", stats.summary());
            println!("  state: {}", replica.state_hash()?);
            if !replica.pending_deletes()?.is_empty() {
                println!();
                println!(
                    "ожидает решения об удалении: {} — syncth pending list",
                    replica.pending_deletes()?.len()
                );
            }
        }

        Command::Discover { seconds } => {
            let found = net::discover(Duration::from_secs(seconds))?;
            if found.is_empty() {
                println!("никого не нашли (оба устройства в одной сети? сервер запущен?)");
            } else {
                for (dev, addr) in found {
                    println!("{dev}\t{addr}");
                }
            }
        }

        Command::Vault { action } => match action {
            VaultCmd::List => {
                let rows = replica.vault_rows()?;
                if rows.is_empty() {
                    println!("корзина пуста");
                } else {
                    println!("в корзине {} трек(ов):", rows.len());
                    for r in rows {
                        println!(
                            "  {}  ({} , {})",
                            r.path,
                            human(r.size),
                            age_days(r.deleted_at)
                        );
                    }
                    println!();
                    println!("Вернуть: syncth vault restore \"<путь>\"");
                }
            }
            VaultCmd::Restore { path } => {
                resolve::restore(&mut replica, &path)?;
                println!("«{path}» возвращён из корзины в библиотеку.");
                println!("Изменение уедет на другие устройства при следующем обмене.");
            }
            VaultCmd::Gc { days } => {
                let report = vault::gc(&replica, days)?;
                println!("корзина очищена");
                println!("  записей удалено: {}", report.rows_removed);
                println!("  файлов удалено:  {}", report.blobs_removed);
                println!("  сирот удалено:   {}", report.orphans_removed);
            }
        },
    }

    Ok(())
}

fn ensure_entry(replica: &Replica, path: &str) -> Result<()> {
    if replica.entry(path)?.is_none() {
        bail!("путь не найден в библиотеке: {path}");
    }
    Ok(())
}

fn absolute(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(p))
    }
}

/// data-dir не должен попасть в скан и в синхронизацию.
fn is_inside(data_dir: &Path, root: &Path) -> bool {
    let d = absolute(data_dir).ok();
    let r = absolute(root).ok();
    match (d, r) {
        (Some(d), Some(r)) => d.starts_with(&r),
        _ => false,
    }
}

fn default_device_id(data_dir: &Path) -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "device".into());
    // Разные реплики на одной машине (тесты, `--data-dir`) не должны
    // получить одинаковый идентификатор.
    let seed = format!("{host}|{}", data_dir.display());
    format!("dev-{}", blake3_short(&seed))
}

fn blake3_short(s: &str) -> String {
    // Короткий стабильный суффикс без лишней зависимости.
    let mut acc: u64 = 1469598103934665603;
    for b in s.as_bytes() {
        acc ^= *b as u64;
        acc = acc.wrapping_mul(1099511628211);
    }
    format!("{acc:016x}")[..8].to_string()
}

fn age_days(deleted_at_ms: u64) -> String {
    let now = syncth_core::Hlc::wall_ms();
    let days = now.saturating_sub(deleted_at_ms) / (24 * 60 * 60 * 1000);
    match days {
        0 => "сегодня".to_string(),
        1 => "1 день назад".to_string(),
        n if n < 5 => format!("{n} дня назад"),
        n => format!("{n} дней назад"),
    }
}

fn human(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b >= K * K * K {
        format!("{:.2} GiB", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.1} MiB", b / (K * K))
    } else if b >= K {
        format!("{:.0} KiB", b / K)
    } else {
        format!("{bytes} B")
    }
}
