use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;

use atomic_write_file::AtomicWriteFile;

type PublicationResult = Result<(u64, Vec<u8>), std::io::Error>;

struct Publication {
    bytes: Vec<u8>,
    result: SyncSender<PublicationResult>,
}

pub(crate) struct CheckpointWriter {
    sender: SyncSender<Option<Publication>>,
    pending: Option<Receiver<PublicationResult>>,
    pending_bytes: u64,
    worker: Option<JoinHandle<()>>,
}

impl CheckpointWriter {
    pub(crate) fn start(target: PathBuf) -> Result<Self, std::io::Error> {
        let (sender, receiver) = sync_channel::<Option<Publication>>(1);
        let worker = std::thread::Builder::new()
            .name("reflex-durability".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                while let Ok(Some(publication)) = receiver.recv() {
                    let result = publish(&target, &publication.bytes)
                        .map(|written| (written, publication.bytes));
                    let _ = publication.result.send(result);
                }
            })?;
        Ok(Self {
            sender,
            pending: None,
            pending_bytes: 0,
            worker: Some(worker),
        })
    }

    pub(crate) fn submit(&mut self, bytes: Vec<u8>) -> Result<(), std::io::Error> {
        self.barrier()?;
        self.pending_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let (result, pending) = sync_channel(1);
        self.sender
            .send(Some(Publication { bytes, result }))
            .map_err(|_| worker_stopped())?;
        self.pending = Some(pending);
        Ok(())
    }

    pub(crate) fn barrier(&mut self) -> Result<u64, std::io::Error> {
        self.barrier_retaining().map(|(written, _)| written)
    }

    pub(crate) fn barrier_retaining(&mut self) -> Result<(u64, Vec<u8>), std::io::Error> {
        let Some(pending) = self.pending.take() else {
            return Ok((0, Vec::new()));
        };
        let result = pending.recv().map_err(|_| worker_stopped())?;
        self.pending_bytes = 0;
        result
    }

    pub(crate) fn pending_bytes(&self) -> u64 {
        self.pending_bytes
    }

    pub(crate) fn finish(mut self) -> Result<(), std::io::Error> {
        self.barrier()?;
        self.sender.send(None).map_err(|_| worker_stopped())?;
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        }
        Ok(())
    }
}

impl Drop for CheckpointWriter {
    fn drop(&mut self) {
        if self.worker.is_none() {
            return;
        }
        let _ = self.barrier();
        let _ = self.sender.send(None);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        }
    }
}

fn worker_stopped() -> std::io::Error {
    std::io::Error::other("Reflex durability worker stopped")
}

fn publish(target: &std::path::Path, bytes: &[u8]) -> Result<u64, std::io::Error> {
    let mut file = AtomicWriteFile::open(target)?;
    if let Err(error) = file.write_all(bytes) {
        let _ = file.discard();
        return Err(error);
    }
    file.commit()?;
    test_fault_point("atomic-rename");
    u64::try_from(bytes.len()).map_err(|_| std::io::Error::other("bundle exceeds u64 bytes"))
}

#[cfg(debug_assertions)]
fn test_fault_point(phase: &str) {
    const PHASE_ENV: &str = "REFLEX_INTERNAL_TEST_FAULT_PHASE";
    const OCCURRENCE_ENV: &str = "REFLEX_INTERNAL_TEST_FAULT_OCCURRENCE";
    static OCCURRENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if std::env::var_os(PHASE_ENV).as_deref() != Some(std::ffi::OsStr::new(phase)) {
        return;
    }
    let expected = std::env::var(OCCURRENCE_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1);
    if OCCURRENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1 == expected {
        std::process::abort();
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
fn test_fault_point(_: &str) {}

#[cfg(test)]
mod tests {
    use super::CheckpointWriter;

    #[test]
    fn durability_barrier_returns_the_submitted_allocation_without_a_caller_clone() {
        let path = std::env::temp_dir().join(format!(
            "reflex-durability-retained-{}-{:?}.bundle",
            std::process::id(),
            std::thread::current().id()
        ));
        let bytes = vec![3, 1, 4, 1, 5, 9];
        let pointer = bytes.as_ptr();
        let mut writer = CheckpointWriter::start(path.clone()).unwrap();

        writer.submit(bytes).unwrap();
        let (written, retained) = writer.barrier_retaining().unwrap();

        assert_eq!(written, 6);
        assert_eq!(retained, [3, 1, 4, 1, 5, 9]);
        assert_eq!(retained.as_ptr(), pointer);
        assert_eq!(std::fs::read(&path).unwrap(), retained);
        writer.finish().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
