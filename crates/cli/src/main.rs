use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use syncth_core::{pipe, scan, Hlc, Replica};

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
    let mut clock = Hlc::default();

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
            println!("инициализировано");
            println!("  устройство: {}", dev);
            println!("  корень:     {}", root.display());
            println!("  данные:     {}", absolute(&cli.data_dir)?.display());
        }

        Command::Scan => {
            let root = replica
                .root()?
                .context("сначала выполните: syncth init --root <путь>")?;
            let report = scan::scan(&root, &mut replica, &mut clock)?;
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
                println!("Удаление НЕ применено. Решение — за вами (см. docs §5.5).");
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
        }

        Command::Export { dir } => {
            let n = pipe::export_ops(&replica, &dir)?;
            println!("экспортировано операций: {n}");
        }

        Command::Import { dir } => {
            let n = pipe::import_ops(&mut replica, &dir)?;
            println!("применено операций: {n}");
            println!("state: {}", replica.state_hash()?);
        }

        Command::Verify => {
            let root = replica.root()?.context("корень не задан")?;
            let mut ok = 0usize;
            let mut bad = 0usize;
            let mut missing = 0usize;
            for e in replica.live_entries()? {
                let abs = root.join(e.path.replace('/', std::path::MAIN_SEPARATOR_STR));
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
