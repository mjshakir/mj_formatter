use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossbeam_channel as crossbeam;

static CLANG_FORMAT_SERVICE: OnceLock<Result<Arc<ClangFormatService>, String>> = OnceLock::new();
static CLANG_FORMAT_LANES: AtomicUsize = AtomicUsize::new(1);

const CLANG_FORMAT_DEADLINE_SECS: u64 = 30;

struct ClangFormatRequest {
    text: String,
    command: String,
    style: String,
    filename: String,
    region: Option<(usize, usize)>,
    response: crossbeam::Sender<Result<String, String>>,
}

pub(crate) struct ClangFormatService {
    senders: Vec<crossbeam::Sender<ClangFormatRequest>>,
    next_lane: AtomicUsize,
}

pub(crate) struct ClangFormatHandle {
    response: crossbeam::Receiver<Result<String, String>>,
}

impl ClangFormatService {
    pub(crate) fn configure(lane_count: usize) {
        let desired = lane_count.max(1);
        let mut current = CLANG_FORMAT_LANES.load(Ordering::Relaxed);
        while current < desired {
            match CLANG_FORMAT_LANES.compare_exchange_weak(
                current,
                desired,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn global() -> Result<Arc<Self>> {
        match CLANG_FORMAT_SERVICE.get_or_init(Self::spawn_service) {
            Ok(service) => Ok(service.clone()),
            Err(message) => Err(anyhow!(message.clone())),
        }
    }

    fn spawn_service() -> Result<Arc<Self>, String> {
        let lane_count = CLANG_FORMAT_LANES.load(Ordering::Relaxed).max(1);
        let mut senders = Vec::with_capacity(lane_count);
        for lane_index in 0..lane_count {
            let (sender, receiver) = crossbeam::unbounded::<ClangFormatRequest>();
            thread::Builder::new()
                .name(format!("clang-format-lane-{lane_index}"))
                .spawn(move || Self::run_lane(receiver))
                .map_err(|err| format!("failed spawning clang-format lane: {err}"))?;
            senders.push(sender);
        }
        Ok(Arc::new(Self {
            senders,
            next_lane: AtomicUsize::new(0),
        }))
    }

    pub(crate) fn dispatch(
        &self,
        text: String,
        command: String,
        style: String,
        filename: String,
        region: Option<(usize, usize)>,
    ) -> Result<ClangFormatHandle> {
        let (response_tx, response_rx) = crossbeam::bounded(1);
        let lane_index = self.next_lane.fetch_add(1, Ordering::Relaxed) % self.senders.len();
        self.senders
            .get(lane_index)
            .ok_or_else(|| anyhow!("clang-format service unavailable"))?
            .send(ClangFormatRequest {
                text,
                command,
                style,
                filename,
                region,
                response: response_tx,
            })
            .map_err(|_| anyhow!("clang-format service unavailable"))?;
        Ok(ClangFormatHandle {
            response: response_rx,
        })
    }

    fn run_lane(receiver: crossbeam::Receiver<ClangFormatRequest>) {
        while let Ok(request) = receiver.recv() {
            let result = Self::execute_clang_format(
                &request.command,
                &request.style,
                &request.filename,
                &request.text,
                request.region,
            );
            let _ = request.response.send(result);
        }
    }

    fn execute_clang_format(
        command: &str,
        style: &str,
        filename: &str,
        text: &str,
        region: Option<(usize, usize)>,
    ) -> Result<String, String> {
        let mut cmd = Command::new(command);
        cmd.arg(format!("-style={style}"))
            .arg(format!("-assume-filename={filename}"));
        if let Some((start, end)) = region {
            cmd.arg(format!("--lines={start}:{end}"));
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("clang_format unavailable: {e}"))?;

        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(text.as_bytes())
                .map_err(|_| "clang_format failed to send stdin".to_string())?;
        }
        drop(child.stdin.take());

        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();

        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_end(&mut stdout_bytes);
        }
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_end(&mut stderr_bytes);
        }

        let timeout = Duration::from_secs(CLANG_FORMAT_DEADLINE_SECS);
        let start = Instant::now();

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
                        return Err(format!("clang_format non-zero exit: {stderr}"));
                    }
                    return Ok(String::from_utf8_lossy(&stdout_bytes).to_string());
                }
                Ok(None) if start.elapsed() > timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("clang_format timed out after 30s".to_string());
                }
                Ok(None) => {
                    std::thread::yield_now();
                }
                Err(err) => return Err(format!("clang_format execution failed: {err}")),
            }
        }
    }
}

impl ClangFormatHandle {
    pub(crate) fn collect_deadline(self, deadline: Instant) -> Result<String, String> {
        match self.response.recv_deadline(deadline) {
            Ok(result) => result,
            Err(crossbeam::RecvTimeoutError::Timeout) => {
                Err("clang_format timed out waiting for service".to_string())
            }
            Err(crossbeam::RecvTimeoutError::Disconnected) => {
                Err("clang_format service disconnected".to_string())
            }
        }
    }
}
