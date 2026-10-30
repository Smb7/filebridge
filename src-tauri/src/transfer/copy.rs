//! Stream one remote file into another. Bytes stay in this process. Nothing
//! here calls `std::fs` or builds a shell command.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::session::ops::is_real_directory;
use crate::session::{LinkError, RawMeta};

use super::plan::TransferPlan;

pub const COPY_CHUNK: u32 = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyError {
    InvalidPath,
    NotConnected,
    NotFound,
    PermissionDenied,
    AlreadyExists,
    NotAFile,
    NotAllowed,
    SamePath,
    Cancelled,
    Failed,
}

impl From<LinkError> for CopyError {
    fn from(error: LinkError) -> Self {
        match error {
            LinkError::InvalidPath => Self::InvalidPath,
            LinkError::NotConnected => Self::NotConnected,
            LinkError::NotFound => Self::NotFound,
            LinkError::PermissionDenied => Self::PermissionDenied,
            LinkError::AlreadyExists => Self::AlreadyExists,
            LinkError::NotEmpty | LinkError::NotAllowed => Self::NotAllowed,
            LinkError::Failed => Self::Failed,
        }
    }
}

pub(crate) trait ChunkIo: Send + Sync {
    fn lstat<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<RawMeta, LinkError>> + Send + 'a>>;

    fn open_read<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, LinkError>> + Send + 'a>>;

    fn read_at<'a>(
        &'a self,
        token: &'a str,
        max: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, LinkError>> + Send + 'a>>;

    fn open_write<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, LinkError>> + Send + 'a>>;

    fn write_at<'a>(
        &'a self,
        token: &'a str,
        data: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>>;

    fn close_file<'a>(
        &'a self,
        token: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>>;

    fn remove_file<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>>;

    fn rename_file<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>>;
}

pub(crate) fn is_regular_file(meta: &RawMeta) -> bool {
    meta.is_file && !meta.is_dir && !meta.is_symlink
}

pub(crate) async fn stream_copy(
    source: &dyn ChunkIo,
    dest: &dyn ChunkIo,
    plan: &TransferPlan,
    cancel: &AtomicBool,
    chunk_size: u32,
    mut progress: impl FnMut(u64, u64),
) -> Result<u64, CopyError> {
    if chunk_size == 0 {
        return Err(CopyError::Failed);
    }
    if plan.same_session && plan.from_path == plan.to_path {
        return Err(CopyError::SamePath);
    }
    let source_meta = source.lstat(&plan.from_path).await?;
    if !is_regular_file(&source_meta) {
        return Err(CopyError::NotAFile);
    }
    match dest.lstat(&plan.to_path).await {
        Ok(meta) if is_real_directory(&meta) => return Err(CopyError::NotAllowed),
        Ok(_) if !plan.overwrite => return Err(CopyError::AlreadyExists),
        Ok(_) | Err(LinkError::NotFound) => {}
        Err(error) => return Err(error.into()),
    }

    let total = source_meta.size;
    progress(0, total);
    if cancel.load(Ordering::SeqCst) {
        return Err(CopyError::Cancelled);
    }

    let read_token = source.open_read(&plan.from_path).await?;
    let write_token = match dest.open_write(&plan.partial_path).await {
        Ok(token) => token,
        Err(error) => {
            let _ = source.close_file(&read_token).await;
            return Err(error.into());
        }
    };

    // One chunk at a time, even across servers. A piped read/write can stall if
    // the writer is waiting on a channel the reader still holds, and two SFTP
    // gates must never be locked together. Bytes still stay in this process.
    let copied = copy_sequential(
        CopyCursor {
            source,
            dest,
            read_token: &read_token,
            write_token: &write_token,
            chunk_size,
            total,
        },
        cancel,
        &mut progress,
    )
    .await;
    let _ = source.close_file(&read_token).await;
    let _ = dest.close_file(&write_token).await;

    let copied = match copied {
        Ok(copied) => copied,
        Err(error) => {
            let _ = dest.remove_file(&plan.partial_path).await;
            return Err(error);
        }
    };
    if cancel.load(Ordering::SeqCst) {
        let _ = dest.remove_file(&plan.partial_path).await;
        return Err(CopyError::Cancelled);
    }
    match commit(dest, plan).await {
        Ok(()) => Ok(copied),
        Err(error) => {
            let _ = dest.remove_file(&plan.partial_path).await;
            Err(error)
        }
    }
}

struct CopyCursor<'a> {
    source: &'a dyn ChunkIo,
    dest: &'a dyn ChunkIo,
    read_token: &'a str,
    write_token: &'a str,
    chunk_size: u32,
    total: u64,
}

async fn copy_sequential(
    cursor: CopyCursor<'_>,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(u64, u64),
) -> Result<u64, CopyError> {
    let mut copied = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(CopyError::Cancelled);
        }
        let chunk = cursor
            .source
            .read_at(cursor.read_token, cursor.chunk_size)
            .await?;
        if chunk.is_empty() {
            return Ok(copied);
        }
        if chunk.len() > cursor.chunk_size as usize {
            return Err(CopyError::Failed);
        }
        cursor.dest.write_at(cursor.write_token, &chunk).await?;
        copied += chunk.len() as u64;
        progress(copied, cursor.total);
    }
}

async fn commit(dest: &dyn ChunkIo, plan: &TransferPlan) -> Result<(), CopyError> {
    match dest.lstat(&plan.to_path).await {
        Err(LinkError::NotFound) => dest.rename_file(&plan.partial_path, &plan.to_path).await?,
        Ok(meta) if is_real_directory(&meta) => return Err(CopyError::NotAllowed),
        Ok(_) if !plan.overwrite => return Err(CopyError::AlreadyExists),
        Ok(_) => {
            dest.rename_file(&plan.to_path, &plan.backup_path).await?;
            if let Err(error) = dest.rename_file(&plan.partial_path, &plan.to_path).await {
                let _ = dest.rename_file(&plan.backup_path, &plan.to_path).await;
                return Err(error.into());
            }
            let _ = dest.remove_file(&plan.backup_path).await;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ChunkIo, CopyError, is_regular_file, stream_copy};
    use crate::session::{LinkError, RawMeta};
    use crate::transfer::plan::{TransferRequest, plan_transfer};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct MemFile {
        meta: RawMeta,
        bytes: Vec<u8>,
    }

    struct MemIo {
        files: Mutex<HashMap<String, MemFile>>,
        opens: Mutex<HashMap<String, Open>>,
        ops: Mutex<Vec<String>>,
        fail_write: AtomicBool,
        reads: AtomicUsize,
    }

    struct Open {
        path: String,
        offset: u64,
        write: bool,
    }

    impl MemIo {
        fn new() -> Self {
            Self {
                files: Mutex::new(HashMap::new()),
                opens: Mutex::new(HashMap::new()),
                ops: Mutex::new(Vec::new()),
                fail_write: AtomicBool::new(false),
                reads: AtomicUsize::new(0),
            }
        }

        fn note(&self, op: &str) {
            self.ops.lock().expect("ops").push(op.to_string());
        }

        fn put(&self, path: &str, bytes: &[u8], kind: &str) {
            let meta = match kind {
                "dir" => RawMeta {
                    is_dir: true,
                    is_file: false,
                    is_symlink: false,
                    size: 0,
                    modified: None,
                },
                "link" => RawMeta {
                    is_dir: false,
                    is_file: false,
                    is_symlink: true,
                    size: 4,
                    modified: None,
                },
                _ => RawMeta {
                    is_dir: false,
                    is_file: true,
                    is_symlink: false,
                    size: bytes.len() as u64,
                    modified: None,
                },
            };
            self.files.lock().expect("files").insert(
                path.to_string(),
                MemFile {
                    meta,
                    bytes: bytes.to_vec(),
                },
            );
        }

        fn bytes(&self, path: &str) -> Option<Vec<u8>> {
            self.files
                .lock()
                .expect("files")
                .get(path)
                .map(|file| file.bytes.clone())
        }

        fn has(&self, path: &str) -> bool {
            self.files.lock().expect("files").contains_key(path)
        }
    }

    impl ChunkIo for MemIo {
        fn lstat<'a>(
            &'a self,
            path: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<RawMeta, LinkError>> + Send + 'a>,
        > {
            self.note(&format!("lstat {path}"));
            let result = self
                .files
                .lock()
                .expect("files")
                .get(path)
                .map(|file| file.meta.clone())
                .ok_or(LinkError::NotFound);
            Box::pin(async move { result })
        }

        fn open_read<'a>(
            &'a self,
            path: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, LinkError>> + Send + 'a>,
        > {
            self.note(&format!("open_read {path}"));
            let result = if self.files.lock().expect("files").contains_key(path) {
                let token = format!("r-{path}");
                self.opens.lock().expect("opens").insert(
                    token.clone(),
                    Open {
                        path: path.to_string(),
                        offset: 0,
                        write: false,
                    },
                );
                Ok(token)
            } else {
                Err(LinkError::NotFound)
            };
            Box::pin(async move { result })
        }

        fn read_at<'a>(
            &'a self,
            token: &'a str,
            max: u32,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<u8>, LinkError>> + Send + 'a>,
        > {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let result = (|| {
                let opens = self.opens.lock().expect("opens");
                let open = opens.get(token).ok_or(LinkError::Failed)?;
                let files = self.files.lock().expect("files");
                let file = files.get(&open.path).ok_or(LinkError::NotFound)?;
                let start = open.offset as usize;
                if start >= file.bytes.len() {
                    return Ok(Vec::new());
                }
                let end = (start + max as usize).min(file.bytes.len());
                Ok(file.bytes[start..end].to_vec())
            })();
            if let Ok(chunk) = &result
                && !chunk.is_empty()
            {
                let mut opens = self.opens.lock().expect("opens");
                if let Some(open) = opens.get_mut(token) {
                    open.offset += chunk.len() as u64;
                }
            }
            Box::pin(async move { result })
        }

        fn open_write<'a>(
            &'a self,
            path: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, LinkError>> + Send + 'a>,
        > {
            self.note(&format!("open_write {path}"));
            assert!(path.starts_with('/'), "write path must be a remote path");
            assert!(!path.contains('\0'));
            let token = format!("w-{path}");
            self.files.lock().expect("files").insert(
                path.to_string(),
                MemFile {
                    meta: RawMeta {
                        is_dir: false,
                        is_file: true,
                        is_symlink: false,
                        size: 0,
                        modified: None,
                    },
                    bytes: Vec::new(),
                },
            );
            self.opens.lock().expect("opens").insert(
                token.clone(),
                Open {
                    path: path.to_string(),
                    offset: 0,
                    write: true,
                },
            );
            Box::pin(async move { Ok(token) })
        }

        fn write_at<'a>(
            &'a self,
            token: &'a str,
            data: &'a [u8],
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LinkError>> + Send + 'a>>
        {
            self.note(&format!("write {}", data.len()));
            if self.fail_write.load(Ordering::SeqCst) {
                return Box::pin(async { Err(LinkError::Failed) });
            }
            let result = (|| {
                let mut opens = self.opens.lock().expect("opens");
                let open = opens.get_mut(token).ok_or(LinkError::Failed)?;
                if !open.write {
                    return Err(LinkError::Failed);
                }
                let mut files = self.files.lock().expect("files");
                let file = files.get_mut(&open.path).ok_or(LinkError::NotFound)?;
                file.bytes.extend_from_slice(data);
                file.meta.size = file.bytes.len() as u64;
                open.offset += data.len() as u64;
                Ok(())
            })();
            Box::pin(async move { result })
        }

        fn close_file<'a>(
            &'a self,
            token: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LinkError>> + Send + 'a>>
        {
            self.opens.lock().expect("opens").remove(token);
            Box::pin(async { Ok(()) })
        }

        fn remove_file<'a>(
            &'a self,
            path: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LinkError>> + Send + 'a>>
        {
            self.note(&format!("remove {path}"));
            self.files.lock().expect("files").remove(path);
            Box::pin(async { Ok(()) })
        }

        fn rename_file<'a>(
            &'a self,
            from: &'a str,
            to: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LinkError>> + Send + 'a>>
        {
            self.note(&format!("rename {from} -> {to}"));
            let result = (|| {
                let mut files = self.files.lock().expect("files");
                if files.contains_key(to) {
                    return Err(LinkError::AlreadyExists);
                }
                let file = files.remove(from).ok_or(LinkError::NotFound)?;
                files.insert(to.to_string(), file);
                Ok(())
            })();
            Box::pin(async move { result })
        }
    }

    fn plan(
        from_session: &str,
        from: &str,
        to_session: &str,
        to: &str,
        overwrite: bool,
    ) -> crate::transfer::plan::TransferPlan {
        plan_transfer(
            &TransferRequest {
                from_session: from_session.to_string(),
                from_path: from.to_string(),
                to_session: to_session.to_string(),
                to_path: to.to_string(),
                overwrite,
            },
            "id-1",
        )
        .expect("plan")
    }

    #[tokio::test]
    async fn streams_chunks_across_sessions_and_leaves_no_partial() {
        let source = MemIo::new();
        let payload = vec![b'a'; 10];
        source.put("/etc/telegraf/cpu.conf", &payload, "file");
        let dest = MemIo::new();
        let plan = plan(
            "ses-a",
            "/etc/telegraf/cpu.conf",
            "ses-b",
            "/var/cpu.conf",
            false,
        );
        let mut seen = Vec::new();
        let copied = stream_copy(
            &source,
            &dest,
            &plan,
            &AtomicBool::new(false),
            4,
            |bytes, total| {
                seen.push((bytes, total));
            },
        )
        .await
        .expect("copy");
        assert_eq!(copied, 10);
        assert_eq!(dest.bytes("/var/cpu.conf"), Some(payload));
        assert!(!dest.has(&plan.partial_path));
        assert!(!dest.has(&plan.backup_path));
        assert!(seen[0] == (0, 10));
        assert!(seen.last().unwrap().0 >= 4);
        assert!(source.reads.load(Ordering::SeqCst) > 1);
        let ops = dest.ops.lock().expect("ops").join("\n");
        assert!(!ops.contains("sh "));
        assert!(!ops.contains("ssh "));
        assert!(!ops.contains("std::fs"));
    }

    #[tokio::test]
    async fn overwrite_replaces_the_destination_only_after_the_copy_finishes() {
        let io = MemIo::new();
        io.put("/etc/cpu.conf", b"new-bytes", "file");
        io.put("/var/cpu.conf", b"old-bytes", "file");
        let plan = plan("ses", "/etc/cpu.conf", "ses", "/var/cpu.conf", true);
        let copied = stream_copy(&io, &io, &plan, &AtomicBool::new(false), 3, |_, _| {})
            .await
            .expect("overwrite");
        assert_eq!(copied, 9);
        assert_eq!(io.bytes("/var/cpu.conf").expect("dest"), b"new-bytes");
        assert_eq!(io.bytes("/etc/cpu.conf").expect("source"), b"new-bytes");
        assert!(!io.has(&plan.partial_path));
        assert!(!io.has(&plan.backup_path));
    }

    #[tokio::test]
    async fn refuses_an_existing_file_a_directory_and_a_symlink_without_writing() {
        let io = MemIo::new();
        io.put("/etc/cpu.conf", b"new", "file");
        io.put("/var/cpu.conf", b"old", "file");
        io.put("/var/folder", b"", "dir");
        io.put("/etc/link", b"", "link");
        let occupied = plan("ses-a", "/etc/cpu.conf", "ses-b", "/var/cpu.conf", false);
        let dest = MemIo::new();
        dest.put("/var/cpu.conf", b"old", "file");
        let source = MemIo::new();
        source.put("/etc/cpu.conf", b"new", "file");
        let error = stream_copy(
            &source,
            &dest,
            &occupied,
            &AtomicBool::new(false),
            4,
            |_, _| {},
        )
        .await
        .expect_err("exists");
        assert_eq!(error, CopyError::AlreadyExists);
        assert_eq!(dest.bytes("/var/cpu.conf").expect("untouched"), b"old");
        assert!(!dest.has(&occupied.partial_path));

        dest.put("/var/folder", b"", "dir");
        let into_dir = plan("ses-a", "/etc/cpu.conf", "ses-b", "/var/folder", true);
        let error = stream_copy(
            &source,
            &dest,
            &into_dir,
            &AtomicBool::new(false),
            4,
            |_, _| {},
        )
        .await
        .expect_err("dir");
        assert_eq!(error, CopyError::NotAllowed);

        source.put("/etc/link", b"", "link");
        let link = plan("ses-a", "/etc/link", "ses-b", "/var/link.conf", false);
        let error = stream_copy(&source, &dest, &link, &AtomicBool::new(false), 4, |_, _| {})
            .await
            .expect_err("link");
        assert_eq!(error, CopyError::NotAFile);
        assert!(!dest.has("/var/link.conf"));
        assert!(!is_regular_file(&RawMeta {
            is_dir: true,
            is_file: false,
            is_symlink: true,
            size: 0,
            modified: None,
        }));
        let _ = io;
    }

    #[tokio::test]
    async fn cancel_removes_the_partial_and_keeps_the_original() {
        let source = MemIo::new();
        source.put("/etc/cpu.conf", b"abcdefghij", "file");
        let dest = MemIo::new();
        dest.put("/var/cpu.conf", b"original", "file");
        let plan = plan("ses-a", "/etc/cpu.conf", "ses-b", "/var/cpu.conf", true);
        let cancel = AtomicBool::new(false);
        let error = stream_copy(&source, &dest, &plan, &cancel, 4, |bytes, _| {
            if bytes > 0 {
                cancel.store(true, Ordering::SeqCst);
            }
        })
        .await
        .expect_err("cancel");
        assert_eq!(error, CopyError::Cancelled);
        assert_eq!(dest.bytes("/var/cpu.conf").expect("original"), b"original");
        assert!(!dest.has(&plan.partial_path));
        assert!(!dest.has(&plan.backup_path));
    }

    #[tokio::test]
    async fn a_failed_write_does_not_replace_the_destination() {
        let io = Arc::new(MemIo::new());
        io.put("/etc/cpu.conf", b"replacement-text", "file");
        io.put("/var/cpu.conf", b"keep-me", "file");
        io.fail_write.store(true, Ordering::SeqCst);
        let plan = plan("ses", "/etc/cpu.conf", "ses", "/var/cpu.conf", true);
        let error = stream_copy(
            io.as_ref(),
            io.as_ref(),
            &plan,
            &AtomicBool::new(false),
            4,
            |_, _| {},
        )
        .await
        .expect_err("write");
        assert_eq!(error, CopyError::Failed);
        assert_eq!(io.bytes("/var/cpu.conf").expect("kept"), b"keep-me");
        assert!(!io.has(&plan.partial_path));
    }

    #[tokio::test]
    async fn an_empty_file_is_still_one_file() {
        let source = MemIo::new();
        source.put("/etc/empty.conf", b"", "file");
        let dest = MemIo::new();
        let plan = plan(
            "ses-a",
            "/etc/empty.conf",
            "ses-b",
            "/var/empty.conf",
            false,
        );
        let copied = stream_copy(&source, &dest, &plan, &AtomicBool::new(false), 8, |_, _| {})
            .await
            .expect("empty");
        assert_eq!(copied, 0);
        assert_eq!(dest.bytes("/var/empty.conf").expect("created"), b"");
    }
}
