//! LAN-транспорт: обмен операциями и блобами напрямую между устройствами.
//!
//! Никакого сервера, никакого TLS-инфраструктуры: TCP по локальной сети,
//! общий токен библиотеки, все блобы проверяются по BLAKE3.
//!
//! Один сеанс — полный двусторонний синк. Обмен асимметричный, чтобы две
//! стороны не писали друг другу одновременно (иначе большой лог операций
//! упёрся бы в буфер сокета и обе зависли):
//!
//! ```text
//! C: Hello                     S: HelloAck
//! C: Ops(мои)                  S: Ops(мои)          ← обе стороны применяют
//!                              S: Want(что нужно S)
//! C: BlobHead + байты ...      C: Done
//! C: Want(что нужно C)         S: BlobHead + байты ...  S: Done
//! ```

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{BufReader, BufWriter, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::hlc::Hlc;
use crate::op::{Op, OpKind};
use crate::path;
use crate::resolve;
use crate::scan;
use crate::store::{EntryRow, Replica};

const MAX_MSG: usize = 256 * 1024 * 1024;
const DISCOVERY_PORT: u16 = 47_888;
const PROTO: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Msg {
    Hello {
        dev: String,
        token: String,
        proto: u32,
    },
    HelloAck {
        dev: String,
        proto: u32,
    },
    Ops {
        ops: Vec<Op>,
    },
    Want {
        blobs: Vec<String>,
    },
    BlobHead {
        blob: String,
        size: u64,
    },
    Done,
    Error {
        message: String,
    },
}

#[derive(Debug, Default, Clone)]
pub struct SyncStats {
    pub applied_remote_ops: usize,
    pub sent_blobs: usize,
    pub received_blobs: usize,
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub placed_local: usize,
    pub conflicts_saved: usize,
}

impl SyncStats {
    pub fn summary(&self) -> String {
        format!(
            "операций: +{}, блобов: принято {} ({}), отдано {} ({}), копий-конфликтов: {}",
            self.applied_remote_ops,
            self.received_blobs,
            human(self.received_bytes),
            self.sent_blobs,
            human(self.sent_bytes),
            self.conflicts_saved
        )
    }
}

// ------------------------------------------------------------------ framing

fn send_msg<W: Write>(w: &mut W, msg: &Msg) -> Result<()> {
    let body = serde_json::to_vec(msg)?;
    anyhow::ensure!(body.len() <= MAX_MSG, "сообщение слишком большое");
    w.write_all(&(body.len() as u32).to_be_bytes())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

fn recv_msg<R: Read>(r: &mut R) -> Result<Msg> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)
        .context("соединение закрыто до начала сообщения")?;
    let n = u32::from_be_bytes(len) as usize;
    anyhow::ensure!(n <= MAX_MSG, "сообщение слишком большое: {n} байт");
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

// ------------------------------------------------------------------ blobs

/// Блобы, которые физически есть на этом устройстве.
fn available_blobs(replica: &Replica) -> Result<HashSet<String>> {
    Ok(replica
        .local_entries()?
        .into_iter()
        .map(|e| e.blob)
        .collect())
}

/// Блобы, нужные для живых путей, которых здесь нет.
///
/// Исключённые вручную («только на других устройствах») не запрашиваются.
fn missing_blobs(replica: &Replica) -> Result<Vec<String>> {
    let have = available_blobs(replica)?;
    let mut want = Vec::new();
    let mut seen = HashSet::new();
    for e in replica.live_entries()? {
        if replica.is_present(&e.path)? || replica.is_ignored(&e.path)? {
            continue;
        }
        if have.contains(&e.blob) || !seen.insert(e.blob.clone()) {
            continue;
        }
        want.push(e.blob);
    }
    Ok(want)
}

/// Первый локальный путь, где лежит блоб.
fn local_path_for(replica: &Replica, root: &Path, blob: &str) -> Result<Option<PathBuf>> {
    for e in replica.local_entries()? {
        if e.blob == blob {
            let p = disk_path(root, &e.path);
            if p.is_file() {
                return Ok(Some(p));
            }
        }
    }
    Ok(None)
}

fn send_blob<W: Write>(w: &mut W, path: &Path, blob: &str) -> Result<u64> {
    let mut f =
        std::fs::File::open(path).with_context(|| format!("открытие {}", path.display()))?;
    let size = f.metadata()?.len();
    send_msg(
        w,
        &Msg::BlobHead {
            blob: blob.to_string(),
            size,
        },
    )?;
    let mut limited = (&mut f).take(size);
    let n = std::io::copy(&mut limited, w)?;
    anyhow::ensure!(
        n == size,
        "файл изменился во время передачи: {}",
        path.display()
    );
    w.flush()?;
    Ok(n)
}

/// Приём одного блоба: временный файл → проверка BLAKE3 → размещение по всем
/// путям, которые ссылаются на этот блоб и которых здесь нет.
fn receive_blob<R: Read>(
    r: &mut R,
    replica: &mut Replica,
    root: &Path,
    blob: &str,
    size: u64,
) -> Result<(u64, usize)> {
    let targets: Vec<EntryRow> = replica
        .live_entries()?
        .into_iter()
        .filter(|e| e.blob == blob)
        .collect();

    if targets.is_empty() {
        // Блоб не нужен — но байты обязаны уйти из сокета.
        let consumed = std::io::copy(&mut r.by_ref().take(size), &mut std::io::sink())?;
        anyhow::ensure!(consumed == size, "соединение закрыто посреди блоба");
        return Ok((consumed, 0));
    }

    let staging = replica
        .data_dir()
        .context("нужен каталог состояния для временных файлов")?
        .join("incoming");
    std::fs::create_dir_all(&staging)?;
    let part = staging.join(format!("{}.part", blob.trim_start_matches("b3:")));
    let _ = std::fs::remove_file(&part);

    let received = (|| -> Result<u64> {
        let mut file = std::fs::File::create(&part)?;
        let mut hasher = blake3::Hasher::new();
        let mut remaining = size;
        let mut buf = vec![0u8; 1024 * 1024];
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            let n = r.read(&mut buf[..want])?;
            if n == 0 {
                bail!("соединение закрыто посреди блоба {blob}");
            }
            file.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
        file.sync_all()?;
        drop(file);

        let got = format!("b3:{}", hasher.finalize().to_hex());
        anyhow::ensure!(got == blob, "хеш не совпал: ожидали {blob}, получили {got}");
        Ok(size)
    })();

    let received = match received {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };

    let mut placed = 0usize;
    for t in &targets {
        if replica.is_ignored(&t.path)? {
            continue;
        }
        let dst = disk_path(root, &t.path);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if dst.is_file() {
            // Файл уже есть. Если он совпадает — ничего не делаем.
            if let Ok(h) = scan::hash_file(&dst) {
                if h == blob {
                    replica.mark_present(&t.path)?;
                    continue;
                }
                // База — последнее состояние, согласованное с пиром.
                // Совпало: локально файл не трогали, это обычное обновление.
                // Не совпало (или базы нет): правили оба — сохраняем копию.
                let base = replica.synced_blob(&t.path)?;
                if base.is_empty() || base != h {
                    save_conflict_copy(replica, root, &dst, &t.path)?;
                }
            }
        }

        std::fs::copy(&part, &dst).with_context(|| format!("размещение {}", dst.display()))?;
        scan::restore_mtime(&dst, t.mtime_ns)?;
        replica.mark_present(&t.path)?;
        placed += 1;
    }

    let _ = std::fs::remove_file(&part);
    Ok((received, placed))
}

/// Убирает занятый путь в conflict-copy и публикует её как обычный файл,
/// чтобы она не потерялась и уехала на другие устройства.
fn save_conflict_copy(
    replica: &mut Replica,
    root: &Path,
    existing: &Path,
    rel: &str,
) -> Result<()> {
    let old_blob = scan::hash_file(existing)?;
    let meta = std::fs::metadata(existing)?;
    let old_mtime = scan::mtime_ns_public(&meta);
    let old_size = meta.len();

    let conflict_rel = path::conflict_name(rel, replica.device_id(), Hlc::wall_ms());
    let conflict_abs = disk_path(root, &conflict_rel);
    if let Some(parent) = conflict_abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(existing, &conflict_abs)
        .with_context(|| format!("сохранение {}", conflict_abs.display()))?;

    let kind = OpKind::EntryPut {
        path: conflict_rel.clone(),
        blob: old_blob,
        size: old_size,
        mtime_ns: old_mtime,
        added_at: replica.next_hlc()?.ms,
    };
    let op = Op::new(
        replica.device_id().to_string(),
        replica.next_hlc()?,
        Op::canonicalize(kind)?,
    );
    replica.apply(&op)?;
    // Файл уже лежит на диске под новым именем.
    replica.mark_present(&conflict_rel)?;
    Ok(())
}

fn disk_path(root: &Path, rel: &str) -> PathBuf {
    resolve::disk_path(root, rel)
}

// ------------------------------------------------------------------ session

/// Применяет чужие операции и сразу доводит их до диска.
fn absorb_ops(replica: &mut Replica, root: &Path, ops: &[Op], stats: &mut SyncStats) -> Result<()> {
    let before: HashSet<String> = replica
        .live_entries()?
        .into_iter()
        .map(|e| e.path)
        .collect();
    let touched = touched_paths(ops);

    stats.applied_remote_ops += replica.apply_all(ops)?;
    invalidate_mismatched(replica, root, &touched)?;
    stats.conflicts_saved += resolve::prune_deleted_files(replica, root)?.len();
    // Подтверждённые удаления могли забрать блоб — пересчитываем цели.
    let after = replica.live_entries()?;
    for e in after {
        if before.contains(&e.path) {
            continue;
        }
        stats.placed_local += place_from_local(replica, root, &e)?;
    }
    Ok(())
}

/// Пути, которых коснулись операции.
fn touched_paths(ops: &[Op]) -> HashSet<String> {
    let mut set = HashSet::new();
    for op in ops {
        match &op.kind {
            OpKind::EntryPut { path, .. } => {
                set.insert(path.clone());
            }
            OpKind::EntryDel { path } => {
                set.insert(path.clone());
            }
            OpKind::EntryMove { from, to, .. } => {
                set.insert(from.clone());
                set.insert(to.clone());
            }
            OpKind::BlobHave { .. } => {}
        }
    }
    set
}

/// «Файл лежит здесь» ещё не значит «лежит именно этот блоб»: после LWW у
/// пути может быть новое содержимое. Иначе мы бы решили, что скачивать нечего,
/// и чужая правка молча осталась бы только в манифесте.
fn invalidate_mismatched(
    replica: &mut Replica,
    root: &Path,
    paths: &HashSet<String>,
) -> Result<()> {
    for p in paths {
        let Some(e) = replica.entry(p)? else { continue };
        if e.deleted || !replica.is_present(p)? {
            continue;
        }
        let abs = disk_path(root, p);
        match scan::hash_file(&abs) {
            Ok(h) if h == e.blob => {}
            _ => replica.mark_absent(p)?,
        }
    }
    Ok(())
}

/// Если блоб уже лежит здесь под другим путём — копируем, не передавая байты.
fn place_from_local(replica: &mut Replica, root: &Path, e: &EntryRow) -> Result<usize> {
    if replica.is_present(&e.path)? || replica.is_ignored(&e.path)? {
        return Ok(0);
    }
    let Some(src) = local_path_for(replica, root, &e.blob)? else {
        return Ok(0);
    };
    let dst = disk_path(root, &e.path);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if src == dst {
        return Ok(0);
    }
    match std::fs::hard_link(&src, &dst) {
        Ok(()) => {}
        Err(_) => {
            std::fs::copy(&src, &dst)?;
        }
    }
    scan::restore_mtime(&dst, e.mtime_ns)?;
    replica.mark_present(&e.path)?;
    Ok(1)
}

/// Отдать блобы, которые просит пир.
fn serve_wants<R: Read, W: Write>(
    r: &mut R,
    w: &mut W,
    replica: &Replica,
    root: &Path,
    stats: &mut SyncStats,
) -> Result<()> {
    match recv_msg(r)? {
        Msg::Want { blobs } => {
            for blob in blobs {
                match local_path_for(replica, root, &blob)? {
                    Some(p) => {
                        stats.sent_bytes += send_blob(w, &p, &blob)?;
                        stats.sent_blobs += 1;
                    }
                    None => bail!("просят блоб {blob}, которого у нас нет"),
                }
            }
            send_msg(w, &Msg::Done)
        }
        other => bail!("ожидали список нужных блобов, получили {other:?}"),
    }
}

/// Запросить и принять блобы, которых нет.
fn request_wants<R: Read, W: Write>(
    r: &mut R,
    w: &mut W,
    replica: &mut Replica,
    root: &Path,
    stats: &mut SyncStats,
) -> Result<()> {
    let want = missing_blobs(replica)?;
    send_msg(w, &Msg::Want { blobs: want })?;
    loop {
        match recv_msg(r)? {
            Msg::BlobHead { blob, size } => {
                let (bytes, placed) = receive_blob(r, replica, root, &blob, size)?;
                stats.received_bytes += bytes;
                stats.received_blobs += 1;
                stats.placed_local += placed;
            }
            Msg::Done => return Ok(()),
            other => bail!("неожиданное сообщение при передаче: {other:?}"),
        }
    }
}

/// Клиентская сторона: подключиться и синхронизироваться.
pub fn sync_with_peer(replica: &mut Replica, addr: &str, token: &str) -> Result<SyncStats> {
    let root = replica.root()?.context("корень библиотеки не задан")?;
    let stream = TcpStream::connect(addr).with_context(|| format!("подключение к {addr}"))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(120)))?;

    let mut w = BufWriter::new(stream.try_clone()?);
    let mut r = BufReader::new(stream);
    let mut stats = SyncStats::default();

    send_msg(
        &mut w,
        &Msg::Hello {
            dev: replica.device_id().to_string(),
            token: token.to_string(),
            proto: PROTO,
        },
    )?;
    match recv_msg(&mut r)? {
        Msg::HelloAck { dev, .. } => {
            anyhow::ensure!(
                dev != replica.device_id(),
                "подключились к самому себе — проверьте адрес"
            );
            tracing::info!("подключено к {dev}");
        }
        Msg::Error { message } => bail!("пир отказал: {message}"),
        other => bail!("неожиданный ответ: {other:?}"),
    }

    // 1. Наши операции — пиру.
    send_msg(
        &mut w,
        &Msg::Ops {
            ops: replica.ops()?,
        },
    )?;
    // 2. Его операции — нам.
    match recv_msg(&mut r)? {
        Msg::Ops { ops } => absorb_ops(replica, &root, &ops, &mut stats)?,
        Msg::Error { message } => bail!("пир вернул ошибку: {message}"),
        other => bail!("неожиданное сообщение: {other:?}"),
    }

    // 3. Сначала отдаём то, что нужно пиру.
    serve_wants(&mut r, &mut w, replica, &root, &mut stats)?;
    // 4. Потом тянем своё.
    request_wants(&mut r, &mut w, replica, &root, &mut stats)?;

    // Сеанс завершён: текущее содержимое обеих сторон — новая база.
    replica.confirm_synced()?;
    Ok(stats)
}

/// Серверная сторона: принять одно соединение и обслужить его.
pub fn serve_stream(stream: TcpStream, replica: &mut Replica, token: &str) -> Result<SyncStats> {
    serve_one(stream, replica, token)
}

fn serve_one(stream: TcpStream, replica: &mut Replica, token: &str) -> Result<SyncStats> {
    let root = replica.root()?.context("корень библиотеки не задан")?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(120)))?;
    let mut w = BufWriter::new(stream.try_clone()?);
    let mut r = BufReader::new(stream);
    let mut stats = SyncStats::default();

    match recv_msg(&mut r)? {
        Msg::Hello {
            token: got,
            proto,
            dev,
        } => {
            if token.is_empty() || got != token {
                send_msg(
                    &mut w,
                    &Msg::Error {
                        message: "неверный токен".into(),
                    },
                )?;
                bail!("неверный токен от устройства {dev}");
            }
            if proto != PROTO {
                send_msg(
                    &mut w,
                    &Msg::Error {
                        message: format!("версия протокола {proto}, ожидаем {PROTO}"),
                    },
                )?;
                bail!("несовместимая версия протокола");
            }
            if dev == replica.device_id() {
                send_msg(
                    &mut w,
                    &Msg::Error {
                        message: "это то же самое устройство".into(),
                    },
                )?;
                bail!("клиент представился нашим же device id");
            }
        }
        other => bail!("ожидали hello, получили {other:?}"),
    }
    send_msg(
        &mut w,
        &Msg::HelloAck {
            dev: replica.device_id().to_string(),
            proto: PROTO,
        },
    )?;

    // 1. Принимаем операции клиента.
    match recv_msg(&mut r)? {
        Msg::Ops { ops } => absorb_ops(replica, &root, &ops, &mut stats)?,
        other => bail!("ожидали операции, получили {other:?}"),
    }
    // 2. Отдаём свои.
    send_msg(
        &mut w,
        &Msg::Ops {
            ops: replica.ops()?,
        },
    )?;

    // 3. Просим своё, 4. отдаём чужое.
    request_wants(&mut r, &mut w, replica, &root, &mut stats)?;
    serve_wants(&mut r, &mut w, replica, &root, &mut stats)?;

    replica.confirm_synced()?;
    Ok(stats)
}

/// Блокирующий сервер: принимает соединения одно за другим.
pub fn serve(
    replica: &mut Replica,
    bind: &str,
    token: &str,
    on_connection: impl Fn(&SyncStats),
) -> Result<()> {
    let listener = TcpListener::bind(bind).with_context(|| format!("bind {bind}"))?;
    let port = listener.local_addr()?.port();
    println!("сервер слушает {bind} (порт {port})");
    println!("устройство: {}", replica.device_id());
    start_discovery(replica.device_id().to_string(), port)?;

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let peer = s.peer_addr().ok();
                match serve_one(s, replica, token) {
                    Ok(stats) => {
                        println!("сеанс {peer:?}: {}", stats.summary());
                        on_connection(&stats);
                    }
                    Err(e) => eprintln!("сеанс {peer:?} прерван: {e:#}"),
                }
            }
            Err(e) => eprintln!("accept: {e}"),
        }
    }
    Ok(())
}

pub struct ServerHandle {
    pub addr: String,
    join: std::thread::JoinHandle<Result<SyncStats>>,
}

impl ServerHandle {
    /// Дождаться ровно одного сеанса.
    pub fn finish(self) -> Result<SyncStats> {
        self.join
            .join()
            .map_err(|_| anyhow::anyhow!("поток сервера упал"))?
    }
}

/// Поднимает сервер на один сеанс в отдельном потоке. Нужен тестам и
/// сценарию «ПК слушает, телефон подключается».
pub fn spawn_one(replica: Replica, bind: &str, token: String) -> Result<ServerHandle> {
    let listener = TcpListener::bind(bind).with_context(|| format!("bind {bind}"))?;
    let addr = listener.local_addr()?.to_string();
    let join = std::thread::spawn(move || {
        let mut replica = replica;
        let (stream, _) = listener.accept()?;
        serve_one(stream, &mut replica, &token)
    });
    Ok(ServerHandle { addr, join })
}

// ------------------------------------------------------------------ discovery

const DISCOVERY_MAGIC: &[u8] = b"SYNCTH-DISCOVER-V1";

pub fn start_discovery(dev: String, port: u16) -> Result<()> {
    std::thread::spawn(move || {
        let Ok(sock) = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)) else {
            return;
        };
        let _ = sock.set_broadcast(true);
        let mut buf = [0u8; 64];
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            if &buf[..n] == DISCOVERY_MAGIC {
                let reply = format!("{{\"dev\":\"{dev}\",\"port\":{port}}}");
                let _ = sock.send_to(reply.as_bytes(), from);
            }
        }
    });
    Ok(())
}

/// Ищет устройства в локальной сети. Возвращает (dev, адрес).
pub fn discover(timeout: Duration) -> Result<Vec<(String, String)>> {
    let sock = UdpSocket::bind(("0.0.0.0", 0))?;
    sock.set_broadcast(true)?;
    sock.send_to(DISCOVERY_MAGIC, ("255.255.255.255", DISCOVERY_PORT))?;
    sock.set_read_timeout(Some(timeout))?;

    let mut found = Vec::new();
    let mut buf = [0u8; 256];
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) {
                    if let (Some(dev), Some(port)) = (
                        v.get("dev").and_then(|x| x.as_str()),
                        v.get("port").and_then(|x| x.as_u64()),
                    ) {
                        found.push((dev.to_string(), format!("{}:{port}", from.ip())));
                    }
                }
            }
            Err(_) => break,
        }
    }
    Ok(found)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_roundtrip() {
        let mut buf = Vec::new();
        send_msg(
            &mut buf,
            &Msg::BlobHead {
                blob: "b3:ab".into(),
                size: 42,
            },
        )
        .unwrap();
        let mut r = &buf[..];
        match recv_msg(&mut r).unwrap() {
            Msg::BlobHead { blob, size } => {
                assert_eq!(blob, "b3:ab");
                assert_eq!(size, 42);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn oversized_message_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(MAX_MSG as u32 + 1).to_be_bytes());
        let err = recv_msg(&mut &buf[..]).unwrap_err().to_string();
        assert!(err.contains("слишком большое"), "{err}");
    }
}
